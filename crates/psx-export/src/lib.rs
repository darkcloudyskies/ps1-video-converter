//! Export: decode a stream and pipe it to an `ffmpeg` process as one NUT stream,
//! which encodes and muxes it. See `docs/export-notes.md`.
//!
//! Video and audio go out in disc order, which is already correctly interleaved,
//! and with timestamps taken from sector positions.

pub mod ffmpeg;
pub mod nut;

use ffmpeg::{Container, Job, Process};
use nut::{NutWriter, Stream};
use psx_disc::SectorSource;
use psx_index::{AudioStream, FrameInfo, VideoStream};
use psx_mdec::{decode_frame, Idct, Planes};
use psx_stream::{demux_frame, AudioSectors};
use psx_xa::XaDecoder;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const PIPE_BUFFER: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Disc(#[from] psx_disc::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("couldn't start {}: {source}", program.display())]
    FfmpegStart {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error("ffmpeg failed ({status}):\n{output}")]
    FfmpegFailed { status: String, output: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("none of the stream's {0} frames could be decoded")]
    NoFrames(usize),
}

pub type Result<T> = std::result::Result<T, Error>;

/// What to encode to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// Lossless FFV1 and FLAC.
    #[default]
    Archive,
    /// 8-bit H.264 and AAC, for players that can't handle anything newer.
    H264,
    /// AV1 and FLAC.
    Av1,
}

/// What to encode the audio to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Lossless, at the native sample rate.
    Flac,
    /// Lossy, resampled to 48 kHz.
    Opus,
    /// Lossy, resampled to 48 kHz.
    Aac,
}

/// How samples go out to FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Yuv420p,
    Yuv420p10,
}

impl PixelFormat {
    /// `accurate` keeps the console's own 8-bit values, and `hq` needs 10 bits to
    /// hold its extra precision, except where the preset is 8-bit whatever the IDCT.
    pub fn for_export(preset: Preset, idct: Idct) -> Self {
        match (preset, idct) {
            (Preset::H264, _) | (_, Idct::Accurate) => PixelFormat::Yuv420p,
            (_, Idct::Hq) => PixelFormat::Yuv420p10,
        }
    }

    fn name(self) -> &'static str {
        match self {
            PixelFormat::Yuv420p => "yuv420p",
            PixelFormat::Yuv420p10 => "yuv420p10le",
        }
    }

    fn fourcc(self) -> [u8; 4] {
        match self {
            PixelFormat::Yuv420p => *b"I420",
            PixelFormat::Yuv420p10 => [b'Y', b'3', 11, 10],
        }
    }
}

impl Preset {
    /// The audio codec to use when none is asked for.
    pub fn default_audio(self) -> AudioCodec {
        match self {
            Preset::Archive | Preset::Av1 => AudioCodec::Flac,
            Preset::H264 => AudioCodec::Aac,
        }
    }

    /// The file extension to use when the output doesn't name one.
    pub fn default_extension(self) -> &'static str {
        match self {
            Preset::Archive | Preset::Av1 => "mkv",
            Preset::H264 => "mp4",
        }
    }

    fn container(self, output: &Path) -> Result<Container> {
        let ext = output
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match (self, ext.as_deref()) {
            (_, Some("mkv")) => Ok(Container::Mkv),
            (Preset::H264 | Preset::Av1, Some("mp4")) => Ok(Container::Mp4),
            (Preset::Archive, _) => Err(Error::Unsupported(format!(
                "the archive preset writes .mkv, so {} won't work",
                output.display()
            ))),
            _ => Err(Error::Unsupported(format!(
                "this preset writes .mkv or .mp4, so {} won't work",
                output.display()
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub preset: Preset,
    pub idct: Idct,
    /// Sample aspect ratio to tag the video with.
    pub sar: (u32, u32),
    /// The audio codec, or `None` for the preset's own.
    pub audio: Option<AudioCodec>,
    /// Write constant frame rate video, at the rate [`constant_rate`] picks.
    pub cfr: bool,
    /// The program to run; see [`ffmpeg::locate`].
    pub ffmpeg: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub frames_written: usize,
    pub audio_packets: u32,
    /// The frame rate as a fraction, if the output is constant frame rate.
    pub constant_rate: Option<(u32, u32)>,
    /// Problems that didn't stop the export, including anything FFmpeg printed.
    pub warnings: Vec<String>,
}

/// How far apart the frames furthest ahead of and behind a rate's grid may be, in
/// sectors, for that rate to count as the stream's. Audio sectors push frames
/// about by 1 or 2 either way.
const CFR_MAX_SPREAD: f64 = 4.0;

/// The frame rate for constant frame rate output, as `(numerator, denominator)`.
/// Games run at a whole number of sectors per frame, or at that rate slowed by
/// 1000/1001 for NTSC, like the 14.985 fps of Parasite Eve. A stream that fits
/// neither gets its exact average. A single frame has no rate, so it gets 15 fps.
pub fn constant_rate(video: &VideoStream) -> (u32, u32) {
    let sps = video.timing.sectors_per_second as u64;
    let frames = &video.frames;
    let (Some(first), Some(last)) = (frames.first(), frames.last()) else {
        return (15, 1);
    };
    let span = (last.last_lba - first.last_lba) as u64;
    let intervals = frames.len() as u64 - 1;
    if span == 0 {
        return (15, 1);
    }
    let whole = ((span + intervals / 2) / intervals).max(1);
    // Sectors per frame as a fraction, for each candidate.
    let spread = |(p, q): (u64, u64)| {
        let (mut low, mut high) = (0i64, 0i64);
        for (k, f) in frames.iter().enumerate() {
            let at = (f.last_lba - first.last_lba) as i64 * q as i64 - k as i64 * p as i64;
            low = low.min(at);
            high = high.max(at);
        }
        (high - low) as f64 / q as f64
    };
    // On a tie the whole number wins, since it's listed first.
    let (p, q) = [(whole, 1), (whole * 1001, 1000)]
        .into_iter()
        .map(|c| (c, spread(c)))
        .filter(|&(_, s)| s <= CFR_MAX_SPREAD)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or((span, intervals), |(c, _)| c);
    reduced(sps * q, p)
}

fn reduced(a: u64, b: u64) -> (u32, u32) {
    let (mut x, mut y) = (a, b);
    while y != 0 {
        (x, y) = (y, x % y);
    }
    ((a / x) as u32, (b / x) as u32)
}

/// How many frames constant frame rate output loses, because another frame lands
/// on the same tick. Rounds the same way as FFmpeg's `fps` filter with `round=near`.
fn frames_lost_to_rate(video: &VideoStream, base: u32, rate: (u32, u32)) -> usize {
    let sps = video.timing.sectors_per_second as u64;
    let tick = |lba: u32| {
        let scale = sps * rate.1 as u64;
        ((lba - base) as u64 * rate.0 as u64 + scale / 2) / scale
    };
    video
        .frames
        .windows(2)
        .filter(|w| tick(w[0].last_lba) == tick(w[1].last_lba))
        .count()
}

/// Append the visible part of a frame as full-range planar YCbCr.
pub fn frame_bytes(p: &Planes, format: PixelFormat, out: &mut Vec<u8>) {
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let luma = p.y.chunks_exact(p.coded_width).take(h).map(|r| &r[..w]);
    let chroma = |plane: &'_ [f64], out: &mut Vec<u8>, put: &dyn Fn(f64, &mut Vec<u8>)| {
        for row in plane.chunks_exact(p.chroma_width()).take(ch) {
            for &s in &row[..cw] {
                put(s, out);
            }
        }
    };
    match format {
        PixelFormat::Yuv420p => {
            let put = |s: f64, out: &mut Vec<u8>| {
                out.push((s + 128.0).round().clamp(0.0, 255.0) as u8);
            };
            luma.flatten().for_each(|&s| put(s, out));
            chroma(&p.cb, out, &put);
            chroma(&p.cr, out, &put);
        }
        PixelFormat::Yuv420p10 => {
            let scale = 1023.0 / 255.0;
            for &s in luma.flatten() {
                let v = ((s + 128.0) * scale).round().clamp(0.0, 1023.0) as u16;
                out.extend_from_slice(&v.to_le_bytes());
            }
            let put = |s: f64, out: &mut Vec<u8>| {
                let v = (s * scale + 512.0).round().clamp(0.0, 1023.0) as u16;
                out.extend_from_slice(&v.to_le_bytes());
            };
            chroma(&p.cb, out, &put);
            chroma(&p.cr, out, &put);
        }
    }
}

/// The audio stream's next sector, decoded when its turn comes.
struct AudioTrack<'a> {
    reader: AudioSectors<'a>,
    decoder: XaDecoder,
    next: Option<psx_disc::RawSector>,
    stream: &'a AudioStream,
    found: u32,
}

/// The sector of the stream's first packet, which all timestamps count from. FFmpeg
/// moves the earliest packet to zero anyway, so this makes our timestamps the same
/// as the output's.
pub fn base_sector(video: &VideoStream, audio: &[&AudioStream]) -> u32 {
    let first_frame = video.frames.first().map_or(video.first_lba, |f| f.last_lba);
    audio
        .iter()
        .map(|a| a.first_lba)
        .fold(first_frame, u32::min)
}

/// Sector position to time-base units. Both clocks count from the same sector.
fn sector_pts(lba: u32, base: u32, rate: u32, sectors_per_second: u32) -> u64 {
    (lba - base) as u64 * rate as u64 / sectors_per_second as u64
}

/// How frames are decoded and sent.
#[derive(Debug, Clone, Copy)]
pub struct Picture {
    pub idct: Idct,
    pub format: PixelFormat,
    pub sar: (u32, u32),
}

/// Decode a stream and write it to `w` as NUT, in disc order. `progress` gets the
/// number of frames handled and the total.
pub fn write_nut<W: Write>(
    w: W,
    src: &mut dyn SectorSource,
    video: &VideoStream,
    audio: &[&AudioStream],
    picture: Picture,
    mut progress: impl FnMut(usize, usize),
) -> Result<(W, ExportReport)> {
    let sps = video.timing.sectors_per_second;
    let base = base_sector(video, audio);

    let mut streams = vec![Stream::Video {
        fourcc: picture.format.fourcc(),
        width: video.width,
        height: video.height,
        sar: picture.sar,
        time_base_den: sps,
    }];
    streams.extend(audio.iter().map(|a| Stream::Audio {
        sample_rate: a.format.sample_rate,
        channels: a.format.channels,
    }));
    let mut nut = NutWriter::new(w, &streams)?;

    let mut tracks = Vec::with_capacity(audio.len());
    for &a in audio {
        let mut reader = AudioSectors::new(a);
        let next = reader.next_sector(src)?;
        tracks.push(AudioTrack {
            reader,
            decoder: XaDecoder::new(a.format.channels == 2, a.format.bits_per_sample),
            next,
            stream: a,
            found: 0,
        });
    }

    let mut report = ExportReport::default();
    let mut buf = Vec::new();
    let mut pcm = Vec::new();
    let mut frames = video.frames.iter();
    let mut next_frame = frames.next();
    let mut frames_done = 0;
    let mut last_frame_lba = None;

    loop {
        let audio_lba = tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| Some((t.next.as_ref()?.lba, i)))
            .min();
        let take_frame = match (next_frame, audio_lba) {
            (None, None) => break,
            (Some(f), Some((lba, _))) => f.last_lba < lba,
            (Some(_), None) => true,
            (None, Some(_)) => false,
        };

        if take_frame {
            let f = next_frame.expect("take_frame needs a frame");
            next_frame = frames.next();
            frames_done += 1;
            progress(frames_done, video.frames.len());
            if last_frame_lba.is_some_and(|last| f.last_lba <= last) {
                report.warnings.push(format!(
                    "skipped frame {}: it ends at or before the previous frame",
                    f.number
                ));
                continue;
            }
            match decode_visible(src, video, f, picture.idct, &mut report)? {
                Some(planes) => {
                    buf.clear();
                    frame_bytes(&planes, picture.format, &mut buf);
                    let pts = (f.last_lba - base) as u64;
                    nut.frame(0, pts, &buf)?;
                    report.frames_written += 1;
                    last_frame_lba = Some(f.last_lba);
                }
                None => continue,
            }
        } else {
            let (_, i) = audio_lba.expect("no frame, so there's audio");
            let t = &mut tracks[i];
            let sector = t.next.take().expect("just found it");
            pcm.clear();
            t.decoder.decode_sector(sector.user_data(), &mut pcm);
            buf.clear();
            buf.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
            let rate = t.stream.format.sample_rate;
            nut.frame(i + 1, sector_pts(sector.lba, base, rate, sps), &buf)?;
            t.found += 1;
            report.audio_packets += 1;
            t.next = t.reader.next_sector(src)?;
        }
    }

    for t in &tracks {
        if t.found != t.stream.sector_count {
            report.warnings.push(format!(
                "audio stream {}: expected {} sectors but found {}",
                t.stream.id, t.stream.sector_count, t.found
            ));
        }
    }
    if report.frames_written == 0 {
        return Err(Error::NoFrames(video.frames.len()));
    }
    Ok((nut.finish()?, report))
}

/// Decode a frame, or record why it has to be skipped. A skipped frame leaves a
/// gap rather than a wrong picture, and the timestamps of the rest don't change.
fn decode_visible(
    src: &mut dyn SectorSource,
    video: &VideoStream,
    f: &FrameInfo,
    idct: Idct,
    report: &mut ExportReport,
) -> Result<Option<Planes>> {
    if (f.width, f.height) != (video.width, video.height) {
        report.warnings.push(format!(
            "skipped frame {}: it's {}x{} in a {}x{} stream",
            f.number, f.width, f.height, video.width, video.height
        ));
        return Ok(None);
    }
    let demuxed = demux_frame(src, video, f)?;
    if demuxed.missing_chunks > 0 {
        report.warnings.push(format!(
            "frame {} is missing {} of {} sectors",
            f.number, demuxed.missing_chunks, f.chunk_count
        ));
    }
    match decode_frame(&demuxed.data, f.width, f.height, idct) {
        Ok(planes) => Ok(Some(planes)),
        Err(e) => {
            report
                .warnings
                .push(format!("skipped frame {}: {e}", f.number));
            Ok(None)
        }
    }
}

/// `dir/name.mkv` to `dir/name.partial.mkv`, keeping the extension FFmpeg expects.
fn partial_path(output: &Path) -> PathBuf {
    let stem = output.file_stem().unwrap_or_default().to_string_lossy();
    let ext = output.extension().unwrap_or_default().to_string_lossy();
    output.with_file_name(format!("{stem}.partial.{ext}"))
}

/// Export a video stream and its audio to `output`. The file only appears once
/// FFmpeg has finished successfully, so a failed export can't replace a good one.
pub fn export_stream(
    src: &mut dyn SectorSource,
    video: &VideoStream,
    audio: &[&AudioStream],
    options: &ExportOptions,
    output: &Path,
    progress: impl FnMut(usize, usize),
) -> Result<ExportReport> {
    let container = options.preset.container(output)?;
    let audio_codec = options.audio.unwrap_or(options.preset.default_audio());
    if options.preset == Preset::Archive && audio_codec != AudioCodec::Flac {
        return Err(Error::Unsupported(
            "the archive preset is lossless, so its audio has to be FLAC".into(),
        ));
    }
    let partial = partial_path(output);
    let format = PixelFormat::for_export(options.preset, options.idct);
    let constant_rate = options.cfr.then(|| constant_rate(video));
    let args = ffmpeg::args(&Job {
        preset: options.preset,
        audio: (!audio.is_empty()).then_some(audio_codec),
        sectors_per_second: video.timing.sectors_per_second,
        constant_rate,
        pix_fmt: format.name(),
        container,
        output: &partial,
    });

    let mut process = Process::spawn(&options.ffmpeg, &args)?;
    let stdin = BufWriter::with_capacity(PIPE_BUFFER, process.take_stdin());
    let picture = Picture {
        idct: options.idct,
        format,
        sar: options.sar,
    };
    let muxed = write_nut(stdin, src, video, audio, picture, progress).map(|(stdin, report)| {
        // Closes the pipe, which is how FFmpeg knows the stream has ended.
        drop(stdin);
        report
    });
    let finished = process.finish();

    let outcome = match (muxed, finished) {
        (Ok(mut report), Ok(lines)) => {
            report.constant_rate = constant_rate;
            if let Some(rate) = constant_rate {
                let lost = frames_lost_to_rate(video, base_sector(video, audio), rate);
                if lost > 0 {
                    report.warnings.push(format!(
                        "constant frame rate dropped {lost} frames that landed on the same tick"
                    ));
                }
            }
            report.warnings.extend(lines);
            Ok(report)
        }
        // A failed write usually just means FFmpeg quit, and it says why.
        (Err(Error::Io(_)), Err(failed)) => Err(failed),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    };
    match outcome {
        Ok(report) => {
            std::fs::rename(&partial, output)?;
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_index::Timing;

    /// A stream with frames ending at these sectors.
    fn stream_at(lbas: &[u32], sps: u32) -> VideoStream {
        let frames: Vec<FrameInfo> = lbas
            .iter()
            .enumerate()
            .map(|(i, &last_lba)| FrameInfo {
                number: i as u32 + 1,
                first_lba: last_lba,
                last_lba,
                chunk_count: 1,
                chunks_found: 1,
                demux_size: 0,
                width: 16,
                height: 16,
                quant_scale: 1,
                version: 2,
            })
            .collect();
        VideoStream {
            id: 0,
            file: None,
            channel: 0,
            first_lba: lbas.first().copied().unwrap_or(0),
            last_lba: lbas.last().copied().unwrap_or(0),
            width: 16,
            height: 16,
            variant: "STRv2".into(),
            frame_count: frames.len(),
            incomplete_frames: 0,
            timing: Timing {
                sectors_per_second: sps,
                speed_source: "audio".into(),
                fps: 0.0,
                frame_sector_deltas: Vec::new(),
            },
            audio: Vec::new(),
            frames,
        }
    }

    #[test]
    fn constant_rate_finds_whole_sector_rates_despite_jitter() {
        let lbas: Vec<u32> = (0..500)
            .map(|k| 100 + k * 10 + [0, 1, 0, 0][k as usize % 4])
            .collect();
        assert_eq!(constant_rate(&stream_at(&lbas, 150)), (15, 1));
        let lbas: Vec<u32> = (0..500).map(|k| k * 5).collect();
        assert_eq!(constant_rate(&stream_at(&lbas, 150)), (30, 1));
        let lbas: Vec<u32> = (0..500).map(|k| k * 10).collect();
        assert_eq!(constant_rate(&stream_at(&lbas, 75)), (15, 2));
    }

    #[test]
    fn constant_rate_finds_the_ntsc_slowdown() {
        // 10.01 sectors per frame, as on Parasite Eve.
        let lbas: Vec<u32> = (0..2000).map(|k| k * 1001 / 100).collect();
        assert_eq!(constant_rate(&stream_at(&lbas, 150)), (15000, 1001));
    }

    #[test]
    fn constant_rate_finds_the_ntsc_slowdown_on_a_short_stream_and_through_wobble() {
        // Too short for the slowdown to rule out 15 fps, so the better fit decides.
        let lbas: Vec<u32> = (0..250).map(|k| k * 1001 / 100).collect();
        assert_eq!(constant_rate(&stream_at(&lbas, 150)), (15000, 1001));
        // A frame 2 sectors early and another 2 late, with the first on the grid.
        let mut lbas: Vec<u32> = (0..2000).map(|k| 100 + k * 1001 / 100).collect();
        lbas[700] -= 2;
        lbas[1400] += 2;
        assert_eq!(constant_rate(&stream_at(&lbas, 150)), (15000, 1001));
    }

    #[test]
    fn constant_rate_falls_back_to_the_exact_average() {
        // Half the stream at 10 sectors per frame, half at 20.
        let mut lbas: Vec<u32> = (0..100).map(|k| k * 10).collect();
        lbas.extend((0..100).map(|k| 1000 + k * 20));
        assert_eq!(
            constant_rate(&stream_at(&lbas, 150)),
            reduced(150 * 199, 2980)
        );
    }

    #[test]
    fn constant_rate_copes_with_a_single_frame_or_none() {
        assert_eq!(constant_rate(&stream_at(&[], 150)), (15, 1));
        assert_eq!(constant_rate(&stream_at(&[40], 150)), (15, 1));
    }

    #[test]
    fn frames_on_the_same_tick_count_as_lost() {
        let even = stream_at(&[0, 10, 20, 30], 150);
        assert_eq!(frames_lost_to_rate(&even, 0, (15, 1)), 0);
        let bunched = stream_at(&[0, 1, 2, 30], 150);
        assert_eq!(frames_lost_to_rate(&bunched, 0, (15, 1)), 2);
    }

    #[test]
    fn h264_is_8_bit_whatever_the_idct() {
        for idct in [Idct::Hq, Idct::Accurate] {
            let f = PixelFormat::for_export(Preset::H264, idct);
            assert_eq!(f, PixelFormat::Yuv420p);
        }
        let f = PixelFormat::for_export(Preset::Av1, Idct::Hq);
        assert_eq!(f, PixelFormat::Yuv420p10);
        let f = PixelFormat::for_export(Preset::Archive, Idct::Accurate);
        assert_eq!(f, PixelFormat::Yuv420p);
    }

    fn planes(y: f64, cb: f64, cr: f64) -> Planes {
        Planes {
            width: 2,
            height: 2,
            coded_width: 16,
            coded_height: 16,
            y: vec![y; 256],
            cb: vec![cb; 64],
            cr: vec![cr; 64],
        }
    }

    #[test]
    fn hq_maps_full_range_to_10_bits() {
        let mut out = Vec::new();
        frame_bytes(
            &planes(-128.0, 0.0, 127.0),
            PixelFormat::Yuv420p10,
            &mut out,
        );
        let s: Vec<u16> = out
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(s, [0, 0, 0, 0, 512, 1021]);

        let mut out = Vec::new();
        frame_bytes(
            &planes(127.0, -128.0, 200.0),
            PixelFormat::Yuv420p10,
            &mut out,
        );
        let s: Vec<u16> = out
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(s[0], 1023);
        assert_eq!(s[4], 0, "overshoot clamps");
        assert_eq!(s[5], 1023, "overshoot clamps");
    }

    #[test]
    fn accurate_stays_8_bit() {
        let mut out = Vec::new();
        frame_bytes(&planes(-128.0, 0.0, 127.0), PixelFormat::Yuv420p, &mut out);
        assert_eq!(out, [0, 0, 0, 0, 128, 255]);
    }

    #[test]
    fn crops_to_the_visible_frame_and_rounds_chroma_up() {
        let mut p = planes(0.0, 0.0, 0.0);
        p.width = 3;
        p.height = 3;
        // A value outside the visible area, in each plane.
        p.y[3] = 50.0;
        p.y[3 * 16] = 50.0;
        p.cb[2] = 50.0;
        p.cr[2 * 8] = 50.0;
        let mut out = Vec::new();
        frame_bytes(&p, PixelFormat::Yuv420p, &mut out);
        assert_eq!(out.len(), 9 + 4 + 4);
        assert!(out.iter().all(|&s| s == 128));
    }

    #[test]
    fn audio_timestamps_count_samples_from_the_base_sector() {
        assert_eq!(sector_pts(1000, 1000, 37_800, 150), 0);
        assert_eq!(sector_pts(1008, 1000, 37_800, 150), 2016);
        assert_eq!(sector_pts(1004, 1000, 18_900, 75), 1008);
    }

    #[test]
    fn presets_accept_only_their_containers() {
        let p = |s: &str| Path::new(s).to_path_buf();
        assert_eq!(
            Preset::Archive.container(&p("a.MKV")).unwrap(),
            Container::Mkv
        );
        assert!(Preset::Archive.container(&p("a.mp4")).is_err());
        assert!(Preset::H264.container(&p("a")).is_err());
        assert_eq!(Preset::H264.container(&p("a.mp4")).unwrap(), Container::Mp4);
        assert_eq!(Preset::Av1.container(&p("a.mkv")).unwrap(), Container::Mkv);
    }

    #[test]
    fn partial_path_keeps_the_extension() {
        assert_eq!(
            partial_path(Path::new("out/PE.D1.mkv")),
            Path::new("out/PE.D1.partial.mkv")
        );
    }
}
