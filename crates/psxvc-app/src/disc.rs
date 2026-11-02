//! An open disc: its index, and frames decoded on request for the viewer.

use anyhow::{Context, Result};
use psx_disc::{BinCueDisc, SectorSource};
use psx_export::base_sector;
use psx_index::{
    default_index_path, scan, AudioStream, DiscIndex, ScanOptions, VideoStream,
    INDEX_FORMAT_VERSION,
};
use psx_mdec::{decode_frame, Idct};
use psx_stream::demux_frame;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Bytes before the pixels in what [`Disc::frame_image`] returns.
pub const IMAGE_HEADER_SIZE: usize = 8;

pub struct Disc {
    pub path: PathBuf,
    pub index: DiscIndex,
    source: Mutex<BinCueDisc>,
}

/// A stream as the disc browser shows it.
#[derive(Debug, Clone, Serialize)]
pub struct StreamSummary {
    pub id: usize,
    pub file: Option<String>,
    pub width: u16,
    pub height: u16,
    pub variant: String,
    pub frame_count: usize,
    pub incomplete_frames: usize,
    pub fps: f64,
    pub constant_rate: bool,
    pub seconds: f64,
    /// One entry per audio stream, e.g. `37800 Hz stereo`.
    pub audio: Vec<String>,
}

/// A frame as the scrubber shows it.
#[derive(Debug, Clone, Serialize)]
pub struct FrameSummary {
    pub number: u32,
    /// When the frame appears in an export, from its sector position.
    pub seconds: f64,
    pub quant_scale: u16,
    pub complete: bool,
}

impl Disc {
    /// Open a disc, using the index beside it if that still fits the disc, or else
    /// scanning and saving one there. `progress` gets sectors scanned and the total.
    pub fn open(path: &Path, progress: impl FnMut(u32, u32)) -> Result<Disc> {
        let mut source =
            BinCueDisc::open(path).with_context(|| format!("opening {}", path.display()))?;
        let index_path = default_index_path(path);
        let index = match saved_index(&index_path, source.sector_count()) {
            Some(index) => index,
            None => {
                let opts = ScanOptions::for_disc(&source, path.display().to_string());
                let index = scan(&mut source, &opts, progress)
                    .with_context(|| format!("scanning {}", path.display()))?;
                // It only saves time next run, so a read-only folder isn't a problem.
                if let Ok(json) = serde_json::to_vec_pretty(&index) {
                    let _ = std::fs::write(&index_path, json);
                }
                index
            }
        };
        Ok(Disc {
            path: path.to_path_buf(),
            index,
            source: Mutex::new(source),
        })
    }

    pub fn name(&self) -> String {
        let stem = self.path.file_stem().unwrap_or_default();
        stem.to_string_lossy().into_owned()
    }

    pub fn video(&self, stream: usize) -> Result<&VideoStream> {
        self.index
            .video
            .iter()
            .find(|v| v.id == stream)
            .with_context(|| format!("no video stream {stream}"))
    }

    fn audio_of(&self, video: &VideoStream) -> Vec<&AudioStream> {
        self.index
            .audio
            .iter()
            .filter(|a| video.audio.contains(&a.id))
            .collect()
    }

    pub fn streams(&self) -> Vec<StreamSummary> {
        self.index
            .video
            .iter()
            .map(|v| StreamSummary {
                id: v.id,
                file: v.file.clone(),
                width: v.width,
                height: v.height,
                variant: v.variant.clone(),
                frame_count: v.frame_count,
                incomplete_frames: v.incomplete_frames,
                fps: v.timing.fps,
                constant_rate: v.timing.is_constant(),
                seconds: if v.timing.fps > 0.0 {
                    v.frame_count as f64 / v.timing.fps
                } else {
                    0.0
                },
                audio: self
                    .audio_of(v)
                    .iter()
                    .map(|a| {
                        let f = a.format;
                        let layout = if f.channels == 2 { "stereo" } else { "mono" };
                        format!("{} Hz {layout}", f.sample_rate)
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn frames(&self, stream: usize) -> Result<Vec<FrameSummary>> {
        let v = self.video(stream)?;
        let base = base_sector(v, &self.audio_of(v));
        let sectors_per_second = v.timing.sectors_per_second as f64;
        Ok(v.frames
            .iter()
            .map(|f| FrameSummary {
                number: f.number,
                seconds: (f.last_lba - base) as f64 / sectors_per_second,
                quant_scale: f.quant_scale,
                complete: f.complete(),
            })
            .collect())
    }

    /// Decode the frame at this position in the stream, for a canvas: width, height,
    /// sectors missing and sectors failing their EDC check, as little-endian `u16`s,
    /// then RGBA pixels. Going through 8-bit RGB loses `hq`'s extra precision, which
    /// an 8-bit screen couldn't show anyway.
    pub fn frame_image(&self, stream: usize, position: usize, idct: Idct) -> Result<Vec<u8>> {
        let v = self.video(stream)?;
        let f = v
            .frames
            .get(position)
            .with_context(|| format!("stream {stream} has no frame at position {position}"))?;
        let demuxed = {
            let mut source = self.source.lock().unwrap();
            demux_frame(&mut *source, v, f)?
        };
        let planes = decode_frame(&demuxed.data, f.width, f.height, idct)
            .with_context(|| format!("decoding frame {}", f.number))?;
        let rgb = planes.to_rgb8();
        let mut out = Vec::with_capacity(IMAGE_HEADER_SIZE + rgb.len() / 3 * 4);
        for field in [
            f.width,
            f.height,
            demuxed.missing_chunks,
            demuxed.bad_edc_chunks,
        ] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for pixel in rgb.chunks_exact(3) {
            out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
        Ok(out)
    }
}

/// The saved index, unless it's missing, unreadable, from another format version, or
/// from a disc of another size.
fn saved_index(path: &Path, sector_count: u32) -> Option<DiscIndex> {
    let data = std::fs::read(path).ok()?;
    let index: DiscIndex = serde_json::from_slice(&data).ok()?;
    let usable = index.format_version == INDEX_FORMAT_VERSION && index.sector_count == sector_count;
    usable.then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_disc::{RawSector, SubHeader, SubMode};
    use psx_index::video::StrHeader;
    use psx_mdec::testutil::encode_frame;
    use psx_mdec::Block;
    use std::io::Write;

    /// A one-file disc with two 32x32 frames, ending at sectors 20 and 30. The first
    /// is mid-grey and the second brighter.
    fn write_disc(dir: &Path) -> PathBuf {
        let bin = dir.join("test.bin");
        let mut f = std::fs::File::create(&bin).unwrap();
        let video = SubHeader {
            file: 1,
            channel: 1,
            submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
            coding_info: 0,
        };
        for lba in 0..40u32 {
            let number = match lba {
                20 => 1,
                30 => 2,
                _ => 0,
            };
            let sector = if number == 0 {
                RawSector::build_mode2(lba, SubHeader::default(), &[])
            } else {
                let blocks: Vec<Block> = (0..4 * 6)
                    .map(|i| {
                        let mut coefs = [0i16; 64];
                        if i % 6 >= 2 {
                            coefs[0] = (number as i16 - 1) * 100;
                        }
                        Block { qscale: 1, coefs }
                    })
                    .collect();
                let frame = encode_frame(&blocks, 1, 2);
                let h = StrHeader {
                    chunk_number: 0,
                    chunk_count: 1,
                    frame_number: number,
                    demux_size: frame.len() as u32,
                    width: 32,
                    height: 32,
                    mdec_code_count: u16::from_le_bytes([frame[0], frame[1]]),
                    magic_3800: 0x3800,
                    quant_scale: 1,
                    version: 2,
                };
                let mut data = vec![0u8; 2048];
                data[..32].copy_from_slice(&h.to_bytes());
                data[32..32 + frame.len()].copy_from_slice(&frame);
                RawSector::build_mode2(lba, video, &data)
            };
            f.write_all(&sector.bytes[..]).unwrap();
        }
        bin
    }

    fn u16_at(image: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([image[at], image[at + 1]])
    }

    #[test]
    fn frames_decode_to_rgba_with_a_header() {
        let dir = tempfile::tempdir().unwrap();
        let disc = Disc::open(&write_disc(dir.path()), |_, _| {}).unwrap();
        let stream = disc.streams()[0].id;
        for idct in [Idct::Hq, Idct::Accurate] {
            let first = disc.frame_image(stream, 0, idct).unwrap();
            assert_eq!(
                (
                    u16_at(&first, 0),
                    u16_at(&first, 2),
                    u16_at(&first, 4),
                    u16_at(&first, 6)
                ),
                (32, 32, 0, 0)
            );
            assert_eq!(first.len(), IMAGE_HEADER_SIZE + 32 * 32 * 4);
            assert_eq!(
                &first[IMAGE_HEADER_SIZE..IMAGE_HEADER_SIZE + 4],
                [128, 128, 128, 255]
            );
            let second = disc.frame_image(stream, 1, idct).unwrap();
            assert!(
                second[IMAGE_HEADER_SIZE] > 128,
                "the second frame is brighter"
            );
        }
        assert!(disc.frame_image(stream, 2, Idct::Hq).is_err());
        assert!(disc.frame_image(99, 0, Idct::Hq).is_err());
    }

    #[test]
    fn frame_times_count_from_the_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let disc = Disc::open(&write_disc(dir.path()), |_, _| {}).unwrap();
        let v = &disc.index.video[0];
        let frames = disc.frames(v.id).unwrap();
        let sps = v.timing.sectors_per_second as f64;
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].seconds, 0.0);
        assert_eq!(frames[1].seconds, 10.0 / sps);
        assert_eq!(frames[1].number, 2);
        assert!(frames[1].complete);
    }

    #[test]
    fn the_index_is_saved_and_only_reused_if_it_fits_the_disc() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_disc(dir.path());
        let scanned = std::cell::Cell::new(false);
        let open = || {
            scanned.set(false);
            Disc::open(&bin, |_, _| scanned.set(true)).unwrap()
        };

        open();
        assert!(scanned.get(), "there's no index yet");
        let index_path = default_index_path(&bin);
        assert!(index_path.exists());

        let disc = open();
        assert!(!scanned.get(), "the saved index is used");
        assert_eq!(disc.streams().len(), 1);

        // From another format version, or another disc that had the same name.
        let good = std::fs::read_to_string(&index_path).unwrap();
        let version = format!("\"format_version\": {INDEX_FORMAT_VERSION}");
        std::fs::write(
            &index_path,
            good.replace(&version, "\"format_version\": 9999"),
        )
        .unwrap();
        open();
        assert!(scanned.get(), "another format version means a new scan");

        let good = std::fs::read_to_string(&index_path).unwrap();
        std::fs::write(
            &index_path,
            good.replace("\"sector_count\": 40", "\"sector_count\": 41"),
        )
        .unwrap();
        open();
        assert!(scanned.get(), "another disc size means a new scan");

        std::fs::write(&index_path, b"not json").unwrap();
        open();
        assert!(scanned.get(), "an unreadable index means a new scan");
    }

    #[test]
    fn a_disc_that_wont_open_gives_an_error_naming_it() {
        let e = Disc::open(Path::new("nowhere/missing.cue"), |_, _| {})
            .err()
            .unwrap();
        assert!(format!("{e:#}").contains("missing.cue"));
    }
}
