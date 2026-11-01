use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use psx_disc::{BinCueDisc, SectorSource};
use psx_export::{export_stream, ffmpeg, ExportOptions, Preset};
use psx_index::{scan, AudioStream, DiscIndex, ScanOptions, VideoStream};
use psx_mdec::{decode_coefficients, decode_frame, reconstruct, Idct};
use psx_stream::{demux_frame, for_each_audio_sector};
use psx_xa::{write_wav, XaDecoder};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "psxvc", version, about = "PlayStation 1 FMV converter")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan a disc image (.cue or raw .bin) for video and audio streams.
    Scan {
        disc: PathBuf,
        /// Where to write the index (default: next to the disc, `<name>.psxidx.json`).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Don't print the progress indicator.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Print the streams in an index file.
    List {
        index: PathBuf,
        /// Also print per-frame details for this video stream id.
        #[arg(long)]
        frames: Option<usize>,
    },
    /// Copy a range of raw sectors into a small standalone BIN/CUE, e.g. one
    /// video stream with its audio, to share for debugging.
    ExtractSectors {
        disc: PathBuf,
        /// Video stream id (from `psxvc list`); includes its audio streams.
        #[arg(long, conflicts_with = "range")]
        video: Option<usize>,
        /// Explicit range as `START:COUNT` sectors.
        #[arg(long)]
        range: Option<String>,
        /// Index to look the stream up in (default: scan the disc).
        #[arg(long)]
        index: Option<PathBuf>,
        /// Output .bin path; a matching .cue is written beside it.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Encode video streams and their audio with FFmpeg, keeping each frame's
    /// exact timing.
    Export {
        disc: PathBuf,
        /// Video stream id (from `psxvc list`).
        #[arg(long, required_unless_present = "all", conflicts_with = "all")]
        stream: Option<usize>,
        /// Export every video stream, with `-o` as the output folder.
        #[arg(long)]
        all: bool,
        #[arg(long, value_enum, default_value_t = PresetArg::Archive)]
        preset: PresetArg,
        #[arg(long, value_enum, default_value_t = IdctArg::Hq)]
        idct: IdctArg,
        /// Sample aspect ratio to tag the video with, as `W:H`.
        #[arg(long, default_value = "1:1")]
        sar: String,
        /// The FFmpeg to run (default: beside `psxvc`, then PATH).
        #[arg(long)]
        ffmpeg: Option<PathBuf>,
        /// Index to look the stream up in (default: `<name>.psxidx.json` beside
        /// the disc, or else scan the disc).
        #[arg(long)]
        index: Option<PathBuf>,
        /// Output file, or folder with `--all` (default: the current folder, with
        /// a name made from the disc, file and stream).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Don't print the progress indicator.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Decode one video frame to a PNG, as a quick visual check.
    Frame {
        disc: PathBuf,
        /// Video stream id (from `psxvc list`).
        #[arg(long)]
        stream: usize,
        /// Frame number (from `psxvc list --frames`).
        #[arg(long)]
        frame: u32,
        #[arg(long, value_enum, default_value_t = IdctArg::Hq)]
        idct: IdctArg,
        /// Index to look the stream up in (default: `<name>.psxidx.json` beside
        /// the disc, or else scan the disc).
        #[arg(long)]
        index: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Decode an audio stream to a WAV file at its native rate.
    Audio {
        disc: PathBuf,
        /// Audio stream id (from `psxvc list`).
        #[arg(long)]
        stream: usize,
        /// Index to look the stream up in (default: `<name>.psxidx.json` beside
        /// the disc, or else scan the disc).
        #[arg(long)]
        index: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Print a checksum per frame of a stream's decoded coefficients and
    /// `accurate` planes, for comparing with tools/jpsxdec-oracle.
    FrameHashes {
        disc: PathBuf,
        /// Video stream id (from `psxvc list`).
        #[arg(long)]
        stream: usize,
        /// Index to look the stream up in (default: `<name>.psxidx.json` beside
        /// the disc, or else scan the disc).
        #[arg(long)]
        index: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum IdctArg {
    /// Double precision with no rounding along the way.
    Hq,
    /// Fixed-point math, as the console's MDEC does.
    Accurate,
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    /// Lossless FFV1 and FLAC in MKV.
    Archive,
    /// H.264 and FLAC in MP4 (or MKV, going by the output's extension).
    H264,
    /// AV1 and FLAC in MKV (or MP4, going by the output's extension).
    Av1,
}

impl From<PresetArg> for Preset {
    fn from(a: PresetArg) -> Preset {
        match a {
            PresetArg::Archive => Preset::Archive,
            PresetArg::H264 => Preset::H264,
            PresetArg::Av1 => Preset::Av1,
        }
    }
}

impl From<IdctArg> for Idct {
    fn from(a: IdctArg) -> Idct {
        match a {
            IdctArg::Hq => Idct::Hq,
            IdctArg::Accurate => Idct::Accurate,
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Scan {
            disc,
            output,
            quiet,
        } => {
            let idx = scan_disc(&disc, quiet)?;
            let out = output.unwrap_or_else(|| default_index_path(&disc));
            std::fs::write(&out, serde_json::to_vec_pretty(&idx)?)
                .with_context(|| format!("writing {}", out.display()))?;
            print_summary(&idx);
            println!("\nIndex written to {}", out.display());
        }
        Cmd::List { index, frames } => {
            let idx = load_index(&index)?;
            print_summary(&idx);
            if let Some(id) = frames {
                print_frames(&idx, id)?;
            }
        }
        Cmd::ExtractSectors {
            disc,
            video,
            range,
            index,
            output,
        } => {
            let (start, count) = match (video, range) {
                (_, Some(r)) => parse_range(&r)?,
                (Some(id), None) => {
                    let idx = match index {
                        Some(p) => load_index(&p)?,
                        None => scan_disc(&disc, false)?,
                    };
                    video_extent(&idx, id)?
                }
                (None, None) => bail!("give either --video <id> or --range START:COUNT"),
            };
            extract(&disc, start, count, &output)?;
        }
        Cmd::Export {
            disc,
            stream,
            all,
            preset,
            idct,
            sar,
            ffmpeg,
            index,
            output,
            quiet,
        } => {
            let options = ExportOptions {
                preset: preset.into(),
                idct: idct.into(),
                sar: parse_sar(&sar)?,
                ffmpeg: ffmpeg::locate(ffmpeg.as_deref()),
            };
            let idx = load_or_scan(&disc, index)?;
            if all {
                export_all(&disc, &idx, &options, output.as_deref(), quiet)?;
            } else {
                let id = stream.context("give either --stream <id> or --all")?;
                let v = video_stream(&idx, id)?;
                let out = output.unwrap_or_else(|| default_export_name(&disc, v, options.preset));
                export_one(&disc, &idx, v, &options, &out, quiet)?;
            }
        }
        Cmd::Frame {
            disc,
            stream,
            frame,
            idct,
            index,
            output,
        } => export_frame(&disc, index, stream, frame, idct.into(), &output)?,
        Cmd::Audio {
            disc,
            stream,
            index,
            output,
        } => export_audio(&disc, index, stream, &output)?,
        Cmd::FrameHashes {
            disc,
            stream,
            index,
        } => print_frame_hashes(&disc, index, stream)?,
    }
    Ok(())
}

/// The given index; else the one beside the disc; else a fresh scan.
fn load_or_scan(disc: &Path, index: Option<PathBuf>) -> Result<DiscIndex> {
    let path = index.unwrap_or_else(|| default_index_path(disc));
    if path.exists() {
        load_index(&path)
    } else {
        scan_disc(disc, false)
    }
}

fn video_stream(idx: &DiscIndex, id: usize) -> Result<&VideoStream> {
    idx.video
        .iter()
        .find(|v| v.id == id)
        .with_context(|| format!("no video stream {id}"))
}

fn open_disc(disc: &Path) -> Result<BinCueDisc> {
    BinCueDisc::open(disc).with_context(|| format!("opening {}", disc.display()))
}

fn parse_sar(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once(':').context("--sar must be W:H")?;
    let sar = (w.trim().parse()?, h.trim().parse()?);
    if sar.0 == 0 || sar.1 == 0 {
        bail!("--sar can't have a zero in it");
    }
    Ok(sar)
}

/// `<disc>-<file>-<stream>.<ext>`, e.g. `PE2 D1-FMV1-FMV016B-3.mkv`.
fn default_export_name(disc: &Path, v: &VideoStream, preset: Preset) -> PathBuf {
    let stem = disc.file_stem().unwrap_or_default().to_string_lossy();
    let file = v
        .file
        .as_deref()
        .map(|f| {
            let f = f.trim_start_matches('/');
            let f = f.rsplit_once('.').map_or(f, |(name, _)| name);
            format!("{}-", f.replace(['/', '\\'], "-"))
        })
        .unwrap_or_default();
    PathBuf::from(format!(
        "{stem}-{file}{}.{}",
        v.id,
        preset.default_extension()
    ))
}

fn export_one(
    disc: &Path,
    idx: &DiscIndex,
    v: &VideoStream,
    options: &ExportOptions,
    output: &Path,
    quiet: bool,
) -> Result<()> {
    let audio: Vec<&AudioStream> = idx
        .audio
        .iter()
        .filter(|a| v.audio.contains(&a.id))
        .collect();
    let mut src = open_disc(disc)?;
    let mut last_pct = u32::MAX;
    let report = export_stream(&mut src, v, &audio, options, output, |done, total| {
        let pct = (done as u64 * 100 / total.max(1) as u64) as u32;
        if !quiet && pct != last_pct {
            last_pct = pct;
            eprint!("\rStream {}: {pct:3}%", v.id);
            let _ = std::io::stderr().flush();
        }
    });
    if !quiet {
        eprintln!();
    }
    let report = report.with_context(|| format!("exporting video stream {}", v.id))?;
    for w in &report.warnings {
        eprintln!("WARNING: stream {}: {w}", v.id);
    }
    println!(
        "Wrote {} of {} frames and {} audio sectors to {}",
        report.frames_written,
        v.frames.len(),
        report.audio_packets,
        output.display()
    );
    Ok(())
}

/// Export every video stream, carrying on past failures so one bad stream doesn't
/// cost the rest of the disc.
fn export_all(
    disc: &Path,
    idx: &DiscIndex,
    options: &ExportOptions,
    output: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let dir = output.unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut failed = 0;
    for v in &idx.video {
        let out = dir.join(default_export_name(disc, v, options.preset));
        if let Err(e) = export_one(disc, idx, v, options, &out, quiet) {
            eprintln!("ERROR: {e:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} of {} streams failed", idx.video.len());
    }
    Ok(())
}

fn export_frame(
    disc: &Path,
    index: Option<PathBuf>,
    stream: usize,
    number: u32,
    idct: Idct,
    output: &Path,
) -> Result<()> {
    let idx = load_or_scan(disc, index)?;
    let v = video_stream(&idx, stream)?;
    let info = v
        .frames
        .iter()
        .find(|f| f.number == number)
        .with_context(|| format!("video stream {stream} has no frame {number}"))?;

    let mut src = open_disc(disc)?;
    let demuxed = demux_frame(&mut src, v, info)?;
    if demuxed.missing_chunks > 0 {
        eprintln!(
            "WARNING: frame {number} is missing {} of {} sectors",
            demuxed.missing_chunks, info.chunk_count
        );
    }
    let planes = decode_frame(&demuxed.data, info.width, info.height, idct)
        .with_context(|| format!("decoding frame {number}"))?;

    let file =
        std::fs::File::create(output).with_context(|| format!("creating {}", output.display()))?;
    let mut png = png::Encoder::new(
        std::io::BufWriter::new(file),
        info.width as u32,
        info.height as u32,
    );
    png.set_color(png::ColorType::Rgb);
    png.set_depth(png::BitDepth::Eight);
    let mut writer = png.write_header()?;
    writer.write_image_data(&planes.to_rgb8())?;
    writer.finish()?;
    println!(
        "Wrote frame {number} ({}x{}) to {}",
        info.width,
        info.height,
        output.display()
    );
    Ok(())
}

fn export_audio(disc: &Path, index: Option<PathBuf>, stream: usize, output: &Path) -> Result<()> {
    let idx = load_or_scan(disc, index)?;
    let a = idx
        .audio
        .iter()
        .find(|a| a.id == stream)
        .with_context(|| format!("no audio stream {stream}"))?;
    let f = a.format;

    let mut src = open_disc(disc)?;
    let mut decoder = XaDecoder::new(f.channels == 2, f.bits_per_sample);
    let mut pcm = Vec::new();
    let found = for_each_audio_sector(&mut src, a, |data| decoder.decode_sector(data, &mut pcm))?;
    if found != a.sector_count {
        eprintln!(
            "WARNING: expected {} audio sectors but found {found}",
            a.sector_count
        );
    }

    let file =
        std::fs::File::create(output).with_context(|| format!("creating {}", output.display()))?;
    let mut out = std::io::BufWriter::new(file);
    write_wav(&mut out, f.sample_rate, f.channels as u16, &pcm)?;
    out.flush()?;
    println!(
        "Wrote {found} sectors, {:.2} s at {} Hz, to {}",
        pcm.len() as f64 / f.channels as f64 / f.sample_rate as f64,
        f.sample_rate,
        output.display()
    );
    Ok(())
}

/// FNV-1a, 64-bit. It only has to catch differences, and it's trivial to
/// reproduce exactly in the Java oracle.
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn add(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// One line per frame: number, coefficient hash, plane hash. A frame that won't
/// decode prints the error class in place of the hashes.
fn print_frame_hashes(disc: &Path, index: Option<PathBuf>, stream: usize) -> Result<()> {
    let idx = load_or_scan(disc, index)?;
    let v = video_stream(&idx, stream)?;
    let mut src = open_disc(disc)?;
    for info in &v.frames {
        let demuxed = demux_frame(&mut src, v, info)?;
        match decode_coefficients(&demuxed.data, info.width, info.height) {
            Ok(blocks) => {
                let mut coefs = Fnv::new();
                for b in &blocks {
                    coefs.add(&[b.qscale]);
                    for c in &b.coefs {
                        coefs.add(&c.to_le_bytes());
                    }
                }
                let p = reconstruct(&blocks, info.width, info.height, Idct::Accurate);
                let mut planes = Fnv::new();
                for plane in [&p.y, &p.cb, &p.cr] {
                    for &s in plane.iter() {
                        planes.add(&(s as i32).to_le_bytes());
                    }
                }
                println!("{} {:016x} {:016x}", info.number, coefs.0, planes.0);
            }
            Err(e) => println!("{} error {e}", info.number),
        }
    }
    Ok(())
}

fn default_index_path(disc: &Path) -> PathBuf {
    let stem = disc.file_stem().unwrap_or_default().to_string_lossy();
    disc.with_file_name(format!("{stem}.psxidx.json"))
}

fn load_index(p: &Path) -> Result<DiscIndex> {
    let data = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("parsing {}", p.display()))
}

fn scan_disc(disc: &Path, quiet: bool) -> Result<DiscIndex> {
    let mut src = BinCueDisc::open(disc).with_context(|| format!("opening {}", disc.display()))?;
    let range = src
        .data_track()
        .map(|t| t.start_lba..t.start_lba + t.sector_count);
    let opts = ScanOptions {
        range,
        source_name: disc.display().to_string(),
    };
    let mut last_pct = u32::MAX;
    let idx = scan(&mut src, &opts, |done, total| {
        let pct = (done as u64 * 100 / total.max(1) as u64) as u32;
        if !quiet && pct != last_pct {
            last_pct = pct;
            eprint!("\rScanning... {pct:3}%");
            let _ = std::io::stderr().flush();
        }
    })?;
    if !quiet {
        eprintln!();
    }
    Ok(idx)
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn print_summary(idx: &DiscIndex) {
    let st = &idx.stats;
    println!(
        "{}: {} sectors ({} Form 1, {} Form 2, {} Mode 1, {} CD-DA/other), {} files",
        idx.source,
        idx.sector_count,
        st.mode2_form1,
        st.mode2_form2,
        st.mode1,
        st.cdda_or_unknown,
        idx.files.len()
    );
    if st.edc_errors > 0 {
        println!(
            "WARNING: {} sectors failed their EDC check (first: {:?}); the rip may be damaged.",
            st.edc_errors, st.edc_error_lbas
        );
    }

    println!("\nVideo streams: {}", idx.video.len());
    if !idx.video.is_empty() {
        println!(
            "  {:>3}  {:<28} {:>9}  {:<8} {:>6} {:>8} {:>7}  {:<10} audio",
            "id", "file", "size", "variant", "frames", "fps", "length", "sectors"
        );
    }
    for v in &idx.video {
        let t = &v.timing;
        let secs = if t.fps > 0.0 {
            v.frame_count as f64 / t.fps
        } else {
            0.0
        };
        let fps = format!("{:.3}{}", t.fps, if t.is_constant() { "" } else { "~" });
        let incomplete = if v.incomplete_frames > 0 {
            format!(" ({} incomplete)", v.incomplete_frames)
        } else {
            String::new()
        };
        println!(
            "  {:>3}  {:<28} {:>9}  {:<8} {:>6} {:>8} {:>7}  {:<10} {:?}{}",
            v.id,
            v.file.as_deref().unwrap_or("-"),
            format!("{}x{}", v.width, v.height),
            v.variant,
            v.frame_count,
            fps,
            fmt_duration(secs),
            format!("{}-{}", v.first_lba, v.last_lba),
            v.audio,
            incomplete
        );
        if t.speed_source != "audio" {
            println!(
                "       note: disc speed assumed {}x (no interleaved audio)",
                t.sectors_per_second / 75
            );
        }
    }

    println!("\nAudio streams: {}", idx.audio.len());
    for a in &idx.audio {
        let f = &a.format;
        println!(
            "  {:>3}  {:<28} XA ch {:<2} {:>5} Hz {} {}-bit  stride {:<4} {:>7}  {}-{}",
            a.id,
            a.file.as_deref().unwrap_or("-"),
            a.xa_channel,
            f.sample_rate,
            if f.channels == 2 { "stereo" } else { "mono  " },
            f.bits_per_sample,
            a.stride.map_or("-".into(), |s| s.to_string()),
            fmt_duration(a.duration_secs),
            a.first_lba,
            a.last_lba
        );
    }
}

fn print_frames(idx: &DiscIndex, id: usize) -> Result<()> {
    let v = idx
        .video
        .iter()
        .find(|v| v.id == id)
        .with_context(|| format!("no video stream {id}"))?;
    println!("\nFrames of video stream {id}:");
    println!(
        "  {:>6} {:>8} {:>8} {:>7} {:>6} {:>6}",
        "frame", "first", "last", "chunks", "bytes", "qscale"
    );
    for f in &v.frames {
        println!(
            "  {:>6} {:>8} {:>8} {:>3}/{:<3} {:>6} {:>6}",
            f.number,
            f.first_lba,
            f.last_lba,
            f.chunks_found,
            f.chunk_count,
            f.demux_size,
            f.quant_scale
        );
    }
    Ok(())
}

fn parse_range(s: &str) -> Result<(u32, u32)> {
    let (a, b) = s.split_once(':').context("range must be START:COUNT")?;
    Ok((a.trim().parse()?, b.trim().parse()?))
}

/// Sector range covering a video stream and its interleaved audio, with a
/// little padding either side.
fn video_extent(idx: &DiscIndex, id: usize) -> Result<(u32, u32)> {
    let v = idx
        .video
        .iter()
        .find(|v| v.id == id)
        .with_context(|| format!("no video stream {id}"))?;
    let mut lo = v.first_lba;
    let mut hi = v.last_lba;
    for a in idx.audio.iter().filter(|a| v.audio.contains(&a.id)) {
        lo = lo.min(a.first_lba);
        hi = hi.max(a.last_lba);
    }
    let lo = lo.saturating_sub(16).max(idx.scanned_range.0);
    let hi = (hi + 16).min(idx.scanned_range.1.saturating_sub(1));
    Ok((lo, hi - lo + 1))
}

fn extract(disc: &Path, start: u32, count: u32, out: &Path) -> Result<()> {
    let mut src = BinCueDisc::open(disc).with_context(|| format!("opening {}", disc.display()))?;
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?,
    );
    let mut written = 0u32;
    let mut lba = start;
    let end = start.saturating_add(count).min(src.sector_count());
    while lba < end {
        let n = 1024.min(end - lba);
        for s in src.read_range(lba, n)? {
            f.write_all(&s.bytes[..])?;
            written += 1;
        }
        lba += n;
    }
    f.flush()?;
    let cue = out.with_extension("cue");
    let bin_name = out.file_name().unwrap().to_string_lossy();
    std::fs::write(
        &cue,
        format!("FILE \"{bin_name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"),
    )?;
    println!(
        "Wrote {written} sectors ({:.1} MB) from {start}..{end} to {} (+ {})",
        written as f64 * 2352.0 / 1e6,
        out.display(),
        cue.display()
    );
    Ok(())
}
