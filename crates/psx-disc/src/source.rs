//! Sector sources. Currently BIN/CUE (and bare `.bin`); CHD can be added
//! behind the same trait later.

use crate::cue::{CueSheet, TrackMode};
use crate::error::{io_err, Error, Result};
use crate::sector::{RawSector, RAW_SECTOR_SIZE};
use serde::Serialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Anything that can hand out raw 2352-byte sectors by absolute sector number.
pub trait SectorSource {
    fn sector_count(&self) -> u32;
    fn read_sector(&mut self, lba: u32) -> Result<RawSector>;

    /// Read `count` consecutive sectors starting at `start` (clamped to the end
    /// of the disc).
    fn read_range(&mut self, start: u32, count: u32) -> Result<Vec<RawSector>> {
        let end = start.saturating_add(count).min(self.sector_count());
        (start..end).map(|l| self.read_sector(l)).collect()
    }
}

/// A track with its position in disc-global sector numbers.
#[derive(Debug, Clone, Serialize)]
pub struct Track {
    pub number: u32,
    pub mode: TrackMode,
    pub start_lba: u32,
    pub sector_count: u32,
}

struct BinFile {
    path: PathBuf,
    file: File,
    /// Global LBA of the file's first sector.
    first_lba: u32,
    sectors: u32,
}

/// A BIN/CUE disc image. Sector numbers are global across all BIN files, with
/// sector 0 being the first sector of the first file (which is how ISO9660
/// addresses the data track on PS1 discs).
pub struct BinCueDisc {
    files: Vec<BinFile>,
    tracks: Vec<Track>,
    total: u32,
}

impl BinCueDisc {
    /// Open a `.cue`, or a bare `.bin` (treated as a single Mode 2 track).
    pub fn open(path: &Path) -> Result<Self> {
        let is_cue = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
        if is_cue {
            Self::from_cue(&CueSheet::load(path)?)
        } else {
            Self::from_bin(path)
        }
    }

    pub fn from_bin(path: &Path) -> Result<Self> {
        let sheet = CueSheet {
            files: vec![path.to_path_buf()],
            tracks: vec![crate::cue::CueTrack {
                number: 1,
                mode: TrackMode::Mode2,
                file_index: 0,
                file_sector_offset: 0,
            }],
        };
        Self::from_cue(&sheet)
    }

    pub fn from_cue(sheet: &CueSheet) -> Result<Self> {
        let mut files = Vec::new();
        let mut lba = 0u32;
        for p in &sheet.files {
            let file = File::open(p).map_err(io_err(p))?;
            let len = file.metadata().map_err(io_err(p))?.len();
            if len % RAW_SECTOR_SIZE as u64 != 0 {
                return Err(Error::Unsupported(format!(
                    "{} is {len} bytes, not a multiple of 2352; is it a cooked 2048-byte ISO? \
                     PS1 video/audio needs a raw BIN rip",
                    p.display()
                )));
            }
            let sectors = (len / RAW_SECTOR_SIZE as u64) as u32;
            files.push(BinFile {
                path: p.clone(),
                file,
                first_lba: lba,
                sectors,
            });
            lba += sectors;
        }
        let total = lba;

        let mut tracks: Vec<Track> = sheet
            .tracks
            .iter()
            .map(|t| Track {
                number: t.number,
                mode: t.mode,
                start_lba: files[t.file_index].first_lba + t.file_sector_offset,
                sector_count: 0,
            })
            .collect();
        for i in 0..tracks.len() {
            let end = tracks.get(i + 1).map_or(total, |n| n.start_lba);
            tracks[i].sector_count = end.saturating_sub(tracks[i].start_lba);
        }
        Ok(BinCueDisc {
            files,
            tracks,
            total,
        })
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The first data (non-audio) track, which holds the filesystem and FMV.
    pub fn data_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.mode != TrackMode::Audio)
    }

    pub fn bin_paths(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|f| f.path.as_path())
    }

    fn locate(&mut self, lba: u32) -> Result<(&mut BinFile, u32)> {
        let f = self
            .files
            .iter_mut()
            .find(|f| lba >= f.first_lba && lba < f.first_lba + f.sectors)
            .ok_or(Error::OutOfRange(lba))?;
        let rel = lba - f.first_lba;
        Ok((f, rel))
    }
}

impl SectorSource for BinCueDisc {
    fn sector_count(&self) -> u32 {
        self.total
    }

    fn read_sector(&mut self, lba: u32) -> Result<RawSector> {
        let (f, rel) = self.locate(lba)?;
        let mut buf = [0u8; RAW_SECTOR_SIZE];
        f.file
            .seek(SeekFrom::Start(rel as u64 * RAW_SECTOR_SIZE as u64))
            .and_then(|_| f.file.read_exact(&mut buf))
            .map_err(io_err(&f.path))?;
        Ok(RawSector::new(lba, &buf))
    }

    fn read_range(&mut self, start: u32, count: u32) -> Result<Vec<RawSector>> {
        let end = start.saturating_add(count).min(self.total);
        let mut out = Vec::with_capacity(end.saturating_sub(start) as usize);
        let mut lba = start;
        while lba < end {
            let (f, rel) = self.locate(lba)?;
            let n = (f.sectors - rel).min(end - lba);
            let mut buf = vec![0u8; n as usize * RAW_SECTOR_SIZE];
            f.file
                .seek(SeekFrom::Start(rel as u64 * RAW_SECTOR_SIZE as u64))
                .and_then(|_| f.file.read_exact(&mut buf))
                .map_err(io_err(&f.path))?;
            for (i, chunk) in buf.chunks_exact(RAW_SECTOR_SIZE).enumerate() {
                out.push(RawSector::new(lba + i as u32, chunk));
            }
            lba += n;
        }
        Ok(out)
    }
}

/// An in-memory sector source, handy for tests and synthetic fixtures.
pub struct MemoryDisc {
    pub sectors: Vec<RawSector>,
}

impl SectorSource for MemoryDisc {
    fn sector_count(&self) -> u32 {
        self.sectors.len() as u32
    }
    fn read_sector(&mut self, lba: u32) -> Result<RawSector> {
        self.sectors
            .get(lba as usize)
            .cloned()
            .ok_or(Error::OutOfRange(lba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sector::{SubHeader, SubMode};
    use std::io::Write;

    fn write_bin(path: &Path, first_lba: u32, n: u32) {
        let mut f = File::create(path).unwrap();
        for i in 0..n {
            let sh = SubHeader {
                submode: SubMode(SubMode::DATA),
                ..Default::default()
            };
            let s = RawSector::build_mode2(first_lba + i, sh, &(first_lba + i).to_le_bytes());
            f.write_all(&s.bytes[..]).unwrap();
        }
    }

    #[test]
    fn multi_file_cue_reads_global_lbas() {
        let dir = tempfile::tempdir().unwrap();
        write_bin(&dir.path().join("a.bin"), 0, 10);
        write_bin(&dir.path().join("b.bin"), 10, 5);
        let cue = dir.path().join("d.cue");
        std::fs::write(
            &cue,
            "FILE \"a.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n\
             FILE \"b.bin\" BINARY\n TRACK 02 MODE2/2352\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        let mut disc = BinCueDisc::open(&cue).unwrap();
        assert_eq!(disc.sector_count(), 15);
        assert_eq!(disc.tracks()[1].start_lba, 10);
        assert_eq!(disc.tracks()[0].sector_count, 10);
        let s = disc.read_sector(12).unwrap();
        assert_eq!(&s.user_data()[..4], &12u32.to_le_bytes());
        let r = disc.read_range(8, 4).unwrap();
        let got: Vec<u32> = r
            .iter()
            .map(|s| u32::from_le_bytes(s.user_data()[..4].try_into().unwrap()))
            .collect();
        assert_eq!(got, vec![8, 9, 10, 11]);
        assert!(disc.read_sector(15).is_err());
    }

    #[test]
    fn cooked_iso_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, vec![0u8; 2048 * 3]).unwrap();
        assert!(matches!(BinCueDisc::open(&p), Err(Error::Unsupported(_))));
    }
}
