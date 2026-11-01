use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use psx_disc::{BinCueDisc, SectorSource};
use psx_index::{scan, DiscIndex, ScanOptions};
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
