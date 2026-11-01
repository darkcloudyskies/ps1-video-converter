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
    /// H.264 and FLAC.
    H264,
    /// AV1 and FLAC.
    Av1,
}

impl Preset {
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
    /// The program to run; see [`ffmpeg::locate`].
    pub ffmpeg: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub frames_written: usize,
    pub audio_packets: u32,
    /// Problems that didn't stop the export, including anything FFmpeg printed.
    pub warnings: Vec<String>,
}

/// The pixel format each IDCT's output goes out in. `accurate` keeps the
/// console's own 8-bit values, and `hq` needs 10 bits to hold its extra precision.
fn pix_fmt(idct: Idct) -> (&'static str, [u8; 4]) {
    match idct {
        Idct::Accurate => ("yuv420p", *b"I420"),
        Idct::Hq => ("yuv420p10le", [b'Y', b'3', 11, 10]),
    }
}

/// Append the visible part of a frame as full-range planar YCbCr, in the pixel
/// format [`pix_fmt`] gives for `idct`.
pub fn frame_bytes(p: &Planes, idct: Idct, out: &mut Vec<u8>) {
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
    match idct {
        Idct::Accurate => {
            let put = |s: f64, out: &mut Vec<u8>| {
                out.push((s + 128.0).round().clamp(0.0, 255.0) as u8);
            };
            luma.flatten().for_each(|&s| put(s, out));
            chroma(&p.cb, out, &put);
            chroma(&p.cr, out, &put);
        }
        Idct::Hq => {
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

/// Decode a stream and write it to `w` as NUT, in disc order. `progress` gets the
/// number of frames handled and the total.
pub fn write_nut<W: Write>(
    w: W,
    src: &mut dyn SectorSource,
    video: &VideoStream,
    audio: &[&AudioStream],
    idct: Idct,
    sar: (u32, u32),
    mut progress: impl FnMut(usize, usize),
) -> Result<(W, ExportReport)> {
    let sps = video.timing.sectors_per_second;
    let base = base_sector(video, audio);

    let (_, fourcc) = pix_fmt(idct);
    let mut streams = vec![Stream::Video {
        fourcc,
        width: video.width,
        height: video.height,
        sar,
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
            match decode_visible(src, video, f, idct, &mut report)? {
                Some(planes) => {
                    buf.clear();
                    frame_bytes(&planes, idct, &mut buf);
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
    let partial = partial_path(output);
    let (pix_fmt, _) = pix_fmt(options.idct);
    let args = ffmpeg::args(&Job {
        preset: options.preset,
        sectors_per_second: video.timing.sectors_per_second,
        pix_fmt,
        container,
        output: &partial,
    });

    let mut process = Process::spawn(&options.ffmpeg, &args)?;
    let stdin = BufWriter::with_capacity(PIPE_BUFFER, process.take_stdin());
    let muxed = write_nut(
        stdin,
        src,
        video,
        audio,
        options.idct,
        options.sar,
        progress,
    )
    .map(|(stdin, report)| {
        // Closes the pipe, which is how FFmpeg knows the stream has ended.
        drop(stdin);
        report
    });
    let finished = process.finish();

    let outcome = match (muxed, finished) {
        (Ok(mut report), Ok(lines)) => {
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
        frame_bytes(&planes(-128.0, 0.0, 127.0), Idct::Hq, &mut out);
        let s: Vec<u16> = out
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(s, [0, 0, 0, 0, 512, 1021]);

        let mut out = Vec::new();
        frame_bytes(&planes(127.0, -128.0, 200.0), Idct::Hq, &mut out);
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
        frame_bytes(&planes(-128.0, 0.0, 127.0), Idct::Accurate, &mut out);
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
        frame_bytes(&p, Idct::Accurate, &mut out);
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
