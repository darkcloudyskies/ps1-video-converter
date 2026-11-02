//! Sector-by-sector disc scan that builds a [`DiscIndex`].

use crate::audio::XaFormat;
use crate::model::*;
use crate::video::StrHeader;
use psx_disc::iso9660::{self, IsoFile};
use psx_disc::{BinCueDisc, RawSector, Result, SectorKind, SectorSource};
use std::collections::HashMap;
use std::ops::Range;

/// Max sectors between two sectors of the same video stream.
const VIDEO_MAX_GAP: u32 = 300;
/// An audio stream is only split at a video's start if it runs at least this
/// many sectors past it on both sides.
const AUDIO_SPLIT_MARGIN: u32 = 32;
const READ_CHUNK: u32 = 1024;
const MAX_REPORTED_EDC_ERRORS: usize = 32;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Sectors to scan. Defaults to the whole source.
    pub range: Option<Range<u32>>,
    /// Name recorded in the index as its source.
    pub source_name: String,
}

impl ScanOptions {
    /// Scan a disc's data track, which holds the filesystem and FMV.
    pub fn for_disc(disc: &BinCueDisc, source_name: String) -> Self {
        ScanOptions {
            range: disc
                .data_track()
                .map(|t| t.start_lba..t.start_lba + t.sector_count),
            source_name,
        }
    }
}

struct OpenVideo {
    file: Option<usize>,
    channel: u8,
    width: u16,
    height: u16,
    last_lba: u32,
    frames: Vec<FrameInfo>,
}

struct OpenAudio {
    file: Option<usize>,
    xa_file: u8,
    xa_channel: u8,
    format: XaFormat,
    first_lba: u32,
    last_lba: u32,
    sectors: u32,
    /// Gap between audio sectors, set by the second sector.
    stride: Option<u32>,
}

impl OpenAudio {
    fn continues_at(&self, lba: u32, format: XaFormat) -> bool {
        let gap = lba - self.last_lba;
        self.format == format
            && match self.stride {
                Some(stride) => gap == stride,
                None => gap == 1 || format.disc_speed(gap).is_some(),
            }
    }

    /// Split at each video start this stream crosses.
    fn split_at_video_starts(self, video_starts: &[u32]) -> Vec<OpenAudio> {
        let mut parts = Vec::new();
        let mut rest = self;
        for &start in video_starts {
            let crosses = rest.first_lba + AUDIO_SPLIT_MARGIN < start
                && rest.last_lba >= start + AUDIO_SPLIT_MARGIN;
            let Some(stride) = rest.stride.filter(|_| crosses) else {
                continue;
            };
            let first_end = rest.first_lba + (start - 1 - rest.first_lba) / stride * stride;
            let second_start = first_end + stride;
            parts.push(OpenAudio {
                last_lba: first_end,
                sectors: (first_end - rest.first_lba) / stride + 1,
                ..rest
            });
            rest.first_lba = second_start;
            rest.sectors = (rest.last_lba - second_start) / stride + 1;
        }
        parts.push(rest);
        parts
    }
}

#[derive(Default)]
struct Scanner {
    files: Vec<IsoFile>,
    stats: SectorStats,
    open_video: Vec<OpenVideo>,
    done_video: Vec<OpenVideo>,
    open_audio: Vec<OpenAudio>,
    done_audio: Vec<OpenAudio>,
}

impl Scanner {
    fn sector(&mut self, s: &RawSector) {
        match s.kind() {
            SectorKind::Mode1 => self.stats.mode1 += 1,
            SectorKind::Mode2Form1 => self.stats.mode2_form1 += 1,
            SectorKind::Mode2Form2 => self.stats.mode2_form2 += 1,
            SectorKind::Cdda | SectorKind::Unknown => {
                self.stats.cdda_or_unknown += 1;
                return;
            }
        }
        if s.edc_ok() == Some(false) {
            self.stats.edc_errors += 1;
            if self.stats.edc_error_lbas.len() < MAX_REPORTED_EDC_ERRORS {
                self.stats.edc_error_lbas.push(s.lba);
            }
        }
        let file = iso9660::file_at(&self.files, s.lba);
        let sh = s.subheader().unwrap_or_default();

        if let Some(h) = StrHeader::parse(s.user_data()) {
            self.stats.video_sectors += 1;
            self.video_sector(s.lba, file, sh.channel, h);
            return;
        }
        if s.kind() == SectorKind::Mode2Form2 && sh.submode.is_audio() {
            if let Some(fmt) = XaFormat::from_coding_info(sh.coding_info) {
                self.stats.audio_sectors += 1;
                self.audio_sector(s.lba, file, sh.file, sh.channel, fmt, sh.submode.is_eof());
            }
        }
    }

    fn video_sector(&mut self, lba: u32, file: Option<usize>, channel: u8, h: StrHeader) {
        // Retire streams that have gone quiet.
        let (stale, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open_video)
            .into_iter()
            .partition(|v| lba - v.last_lba > VIDEO_MAX_GAP);
        self.done_video.extend(stale);
        self.open_video = open;

        let pos = self.open_video.iter().position(|v| {
            let last = v.frames.last().expect("streams start with a frame");
            v.file == file
                && v.channel == channel
                && v.width == h.width
                && v.height == h.height
                && h.frame_number >= last.number
        });
        let v = match pos {
            Some(i) => &mut self.open_video[i],
            None => {
                // Same key but frame numbers went backwards: a new stream.
                if let Some(i) = self
                    .open_video
                    .iter()
                    .position(|v| v.file == file && v.channel == channel)
                {
                    let old = self.open_video.swap_remove(i);
                    self.done_video.push(old);
                }
                self.open_video.push(OpenVideo {
                    file,
                    channel,
                    width: h.width,
                    height: h.height,
                    last_lba: lba,
                    frames: Vec::new(),
                });
                self.open_video.last_mut().unwrap()
            }
        };
        v.last_lba = lba;
        match v.frames.last_mut() {
            Some(f) if f.number == h.frame_number => {
                f.last_lba = lba;
                f.chunks_found = f.chunks_found.saturating_add(1).min(f.chunk_count);
            }
            _ => v.frames.push(FrameInfo {
                number: h.frame_number,
                first_lba: lba,
                last_lba: lba,
                chunk_count: h.chunk_count,
                chunks_found: 1,
                demux_size: h.demux_size,
                width: h.width,
                height: h.height,
                quant_scale: h.quant_scale,
                version: h.version,
            }),
        }
    }

    fn audio_sector(
        &mut self,
        lba: u32,
        file: Option<usize>,
        xa_file: u8,
        xa_channel: u8,
        format: XaFormat,
        eof: bool,
    ) {
        let pos = self
            .open_audio
            .iter()
            .position(|a| a.file == file && a.xa_file == xa_file && a.xa_channel == xa_channel);
        let i = match pos {
            Some(i) if self.open_audio[i].continues_at(lba, format) => {
                let a = &mut self.open_audio[i];
                a.stride = Some(lba - a.last_lba);
                a.last_lba = lba;
                a.sectors += 1;
                i
            }
            other => {
                if let Some(i) = other {
                    let old = self.open_audio.swap_remove(i);
                    self.done_audio.push(old);
                }
                self.open_audio.push(OpenAudio {
                    file,
                    xa_file,
                    xa_channel,
                    format,
                    first_lba: lba,
                    last_lba: lba,
                    sectors: 1,
                    stride: None,
                });
                self.open_audio.len() - 1
            }
        };
        if eof {
            let a = self.open_audio.swap_remove(i);
            self.done_audio.push(a);
        }
    }

    fn finish(mut self, source: String, sector_count: u32, range: (u32, u32)) -> DiscIndex {
        self.done_video.append(&mut self.open_video);
        self.done_audio.append(&mut self.open_audio);
        let name = |f: Option<usize>| f.map(|i| self.files[i].path.clone());

        let mut video_src = std::mem::take(&mut self.done_video);
        video_src.sort_by_key(|v| v.frames[0].first_lba);
        let video_starts: Vec<u32> = video_src.iter().map(|v| v.frames[0].first_lba).collect();

        let mut audio_src: Vec<OpenAudio> = std::mem::take(&mut self.done_audio)
            .into_iter()
            .flat_map(|a| a.split_at_video_starts(&video_starts))
            .collect();
        audio_src.sort_by_key(|a| (a.first_lba, a.xa_channel));
        let audio: Vec<AudioStream> = audio_src
            .into_iter()
            .enumerate()
            .map(|(id, a)| AudioStream {
                id,
                file: name(a.file),
                xa_file: a.xa_file,
                xa_channel: a.xa_channel,
                first_lba: a.first_lba,
                last_lba: a.last_lba,
                sector_count: a.sectors,
                format: a.format,
                stride: a.stride,
                duration_secs: a.sectors as f64 * a.format.samples_per_channel_per_sector() as f64
                    / a.format.sample_rate as f64,
            })
            .collect();

        let video = video_src
            .into_iter()
            .enumerate()
            .map(|(id, v)| {
                let first_lba = v.frames[0].first_lba;
                let linked: Vec<usize> = audio
                    .iter()
                    .filter(|a| {
                        a.file == name(v.file)
                            && overlap(a.first_lba, a.last_lba, first_lba, v.last_lba) > 0
                    })
                    .map(|a| a.id)
                    .collect();
                let timing = timing(&v.frames, linked.iter().map(|&i| &audio[i]));
                VideoStream {
                    id,
                    file: name(v.file),
                    channel: v.channel,
                    first_lba,
                    last_lba: v.last_lba,
                    width: v.width,
                    height: v.height,
                    variant: v.frames[0].variant(),
                    frame_count: v.frames.len(),
                    incomplete_frames: v.frames.iter().filter(|f| !f.complete()).count(),
                    timing,
                    audio: linked,
                    frames: v.frames,
                }
            })
            .collect();

        DiscIndex {
            format_version: INDEX_FORMAT_VERSION,
            source,
            sector_count,
            scanned_range: range,
            stats: self.stats,
            files: self.files,
            video,
            audio,
        }
    }
}

impl FrameInfo {
    fn variant(&self) -> String {
        StrHeader {
            chunk_number: 0,
            chunk_count: self.chunk_count,
            frame_number: self.number,
            demux_size: self.demux_size,
            width: self.width,
            height: self.height,
            mdec_code_count: 0,
            magic_3800: 0x3800,
            quant_scale: self.quant_scale,
            version: self.version,
        }
        .variant()
    }
}

fn overlap(a0: u32, a1: u32, b0: u32, b1: u32) -> u32 {
    (a1.min(b1) + 1).saturating_sub(a0.max(b0))
}

fn timing<'a>(frames: &[FrameInfo], audio: impl Iterator<Item = &'a AudioStream>) -> Timing {
    // Infer disc speed from the first interleaved audio stream with a stride.
    let mut inferred = audio.filter_map(|a| a.format.disc_speed(a.stride?));
    let (sps, source) = match inferred.next() {
        Some(s) => (s, "audio"),
        None => (150, "assumed"),
    };

    let mut deltas: HashMap<u32, u32> = HashMap::new();
    for w in frames.windows(2) {
        *deltas.entry(w[1].last_lba - w[0].last_lba).or_default() += 1;
    }
    let mut frame_sector_deltas: Vec<(u32, u32)> = deltas.into_iter().collect();
    frame_sector_deltas.sort_by_key(|&(d, n)| (std::cmp::Reverse(n), d));

    let span = frames.last().map_or(0, |l| l.last_lba - frames[0].last_lba);
    let fps = if frames.len() >= 2 && span > 0 {
        (frames.len() - 1) as f64 * sps as f64 / span as f64
    } else {
        0.0
    };
    Timing {
        sectors_per_second: sps,
        speed_source: source.into(),
        fps,
        frame_sector_deltas,
    }
}

/// Scan a disc and build its index.
pub fn scan(
    src: &mut dyn SectorSource,
    opts: &ScanOptions,
    mut progress: impl FnMut(u32, u32),
) -> Result<DiscIndex> {
    let range = opts.range.clone().unwrap_or(0..src.sector_count());
    let files = iso9660::read_files(src, range.start).unwrap_or_default();
    let mut sc = Scanner {
        files,
        ..Default::default()
    };
    let mut lba = range.start;
    while lba < range.end {
        let n = READ_CHUNK.min(range.end - lba);
        for s in src.read_range(lba, n)? {
            sc.sector(&s);
        }
        lba += n;
        progress(lba - range.start, range.end - range.start);
    }
    Ok(sc.finish(
        opts.source_name.clone(),
        src.sector_count(),
        (range.start, range.end),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::HEADER_SIZE;
    use psx_disc::iso9660::testutil::tiny_disc;
    use psx_disc::{SubHeader, SubMode};

    fn video_sector(lba: u32, frame: u32, chunk: u16, chunks: u16) -> RawSector {
        let h = StrHeader {
            chunk_number: chunk,
            chunk_count: chunks,
            frame_number: frame,
            demux_size: 2016 * chunks as u32,
            width: 320,
            height: 240,
            mdec_code_count: 1000,
            magic_3800: 0x3800,
            quant_scale: 4,
            version: 2,
        };
        let mut data = vec![0u8; 2048];
        data[..HEADER_SIZE].copy_from_slice(&h.to_bytes());
        let sh = SubHeader {
            file: 1,
            channel: 1,
            submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
            coding_info: 0,
        };
        RawSector::build_mode2(lba, sh, &data)
    }

    fn audio_sector(lba: u32, eof: bool) -> RawSector {
        let mut sm = SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME;
        if eof {
            sm |= SubMode::END_OF_FILE;
        }
        let sh = SubHeader {
            file: 1,
            channel: 0,
            submode: SubMode(sm),
            coding_info: 0x01, // 37.8 kHz stereo 4-bit
        };
        RawSector::build_mode2(lba, sh, &[0u8; 2304])
    }

    /// Lay out OPEN.STR at sectors 30..70 as a typical 2× stream:
    /// every 8th sector is audio, frames are 5 chunks each.
    fn disc_with_stream() -> psx_disc::source::MemoryDisc {
        let mut disc = tiny_disc(80);
        let mut frame = 1;
        let mut chunk = 0;
        for lba in 30..70u32 {
            disc.sectors[lba as usize] = if (lba - 30) % 8 == 7 {
                audio_sector(lba, lba == 69)
            } else {
                let s = video_sector(lba, frame, chunk, 5);
                chunk += 1;
                if chunk == 5 {
                    chunk = 0;
                    frame += 1;
                }
                s
            };
        }
        disc
    }

    #[test]
    fn finds_interleaved_video_and_audio() {
        let mut disc = disc_with_stream();
        let idx = scan(
            &mut disc,
            &ScanOptions {
                range: None,
                source_name: "test".into(),
            },
            |_, _| {},
        )
        .unwrap();

        assert_eq!(idx.stats.edc_errors, 0);
        assert_eq!(idx.audio.len(), 1);
        let a = &idx.audio[0];
        assert_eq!(a.file.as_deref(), Some("/MOVIE/OPEN.STR"));
        assert_eq!(a.sector_count, 5);
        assert_eq!(a.stride, Some(8));

        assert_eq!(idx.video.len(), 1);
        let v = &idx.video[0];
        assert_eq!(v.file.as_deref(), Some("/MOVIE/OPEN.STR"));
        assert_eq!((v.width, v.height), (320, 240));
        assert_eq!(v.variant, "STRv2");
        assert_eq!(v.audio, vec![0]);
        assert_eq!(v.timing.sectors_per_second, 150);
        assert_eq!(v.timing.speed_source, "audio");
        // 35 video sectors / 5 chunks = 7 frames, all complete.
        assert_eq!(v.frame_count, 7);
        assert_eq!(v.incomplete_frames, 0);
        assert!(v.frames.windows(2).all(|w| w[1].number == w[0].number + 1));
    }

    #[test]
    fn stride_change_starts_new_audio_stream() {
        let mut disc = tiny_disc(80);
        for lba in [30, 38, 46, 60, 68] {
            disc.sectors[lba as usize] = audio_sector(lba, false);
        }
        let idx = scan(
            &mut disc,
            &ScanOptions {
                range: None,
                source_name: "t".into(),
            },
            |_, _| {},
        )
        .unwrap();
        let ranges: Vec<_> = idx
            .audio
            .iter()
            .map(|a| (a.first_lba, a.last_lba))
            .collect();
        assert_eq!(ranges, vec![(30, 46), (60, 68)]);
        assert_eq!(idx.audio[0].sector_count, 3);
        assert_eq!(idx.audio[0].stride, Some(8));
    }

    #[test]
    fn audio_splits_at_video_starts_it_crosses() {
        let audio = OpenAudio {
            file: None,
            xa_file: 1,
            xa_channel: 1,
            format: XaFormat::from_coding_info(0x01).unwrap(),
            first_lba: 107,
            last_lba: 507,
            sectors: 51,
            stride: Some(8),
        };
        // 100 and 490 are too close to the ends to split at.
        let parts = audio.split_at_video_starts(&[100, 300, 490]);
        let got: Vec<_> = parts
            .iter()
            .map(|a| (a.first_lba, a.last_lba, a.sectors))
            .collect();
        assert_eq!(got, vec![(107, 299, 25), (307, 507, 26)]);
    }

    #[test]
    fn frame_number_reset_starts_new_stream() {
        let mut disc = tiny_disc(80);
        for (i, lba) in (30..40u32).enumerate() {
            disc.sectors[lba as usize] = video_sector(lba, 1 + i as u32, 0, 1);
        }
        for (i, lba) in (40..50u32).enumerate() {
            disc.sectors[lba as usize] = video_sector(lba, 1 + i as u32, 0, 1);
        }
        let idx = scan(
            &mut disc,
            &ScanOptions {
                range: None,
                source_name: "t".into(),
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(idx.video.len(), 2);
        assert_eq!(idx.video[1].first_lba, 40);
        assert_eq!(idx.video[0].timing.fps, 150.0);
        assert!(idx.video[0].timing.is_constant());
    }

    #[test]
    fn missing_chunk_marks_frame_incomplete() {
        let mut disc = tiny_disc(80);
        disc.sectors[30] = video_sector(30, 1, 0, 3);
        disc.sectors[31] = video_sector(31, 1, 1, 3);
        // chunk 2 lost
        disc.sectors[33] = video_sector(33, 2, 0, 1);
        let idx = scan(
            &mut disc,
            &ScanOptions {
                range: None,
                source_name: "t".into(),
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(idx.video[0].incomplete_frames, 1);
        assert!(!idx.video[0].frames[0].complete());
    }
}
