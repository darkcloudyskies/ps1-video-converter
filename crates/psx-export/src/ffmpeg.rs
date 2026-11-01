//! Running FFmpeg. Everything that knows about its command line or its process
//! lives here, so the rest of the code doesn't depend on how it's run.

use crate::{Error, Preset, Result};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

/// How many lines of FFmpeg's output to keep for error messages.
const KEPT_LINES: usize = 40;

/// The FFmpeg to run: `explicit` if given, else one beside this program, else
/// whatever's on PATH.
pub fn locate(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let name = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(&name)));
    beside
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

/// What FFmpeg needs to know about the stream and where it's going.
pub(crate) struct Job<'a> {
    pub preset: Preset,
    pub sectors_per_second: u32,
    pub pix_fmt: &'static str,
    pub container: Container,
    pub output: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    Mkv,
    Mp4,
}

/// The full command line, reading one NUT stream from stdin.
pub(crate) fn args(job: &Job) -> Vec<OsString> {
    let sps = job.sectors_per_second;
    // -color_range before -i applies to the input, separately from the output's.
    let mut a: Vec<String> = [
        "-y",
        "-hide_banner",
        "-loglevel",
        "warning",
        "-color_range",
        "pc",
        "-f",
        "nut",
        "-i",
        "pipe:0",
        "-map",
        "0",
        "-fps_mode",
        "passthrough",
    ]
    .map(String::from)
    .to_vec();
    // Without this, FFmpeg rounds PTS to the encoder's guessed frame rate and
    // silently produces constant frame rate.
    a.extend(["-enc_time_base:v".into(), format!("1:{sps}")]);
    // Without setparams, primaries and transfer come out `unknown`.
    a.extend([
        "-vf".into(),
        "setparams=color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m:range=pc"
            .into(),
    ]);
    a.extend(
        [
            "-color_primaries",
            "smpte170m",
            "-color_trc",
            "smpte170m",
            "-colorspace",
            "smpte170m",
            "-color_range",
            "pc",
            "-chroma_sample_location",
            "2",
            "-pix_fmt",
            job.pix_fmt,
        ]
        .map(String::from),
    );
    let codec: &[&str] = match job.preset {
        Preset::Archive => &["-c:v", "ffv1", "-level", "3", "-g", "1", "-slicecrc", "1"],
        Preset::H264 => &["-c:v", "libx264", "-crf", "14"],
        Preset::Av1 => &["-c:v", "libsvtav1", "-crf", "20", "-preset", "6"],
    };
    a.extend(codec.iter().map(|s| s.to_string()));
    a.extend(["-c:a".into(), "flac".into()]);
    match job.container {
        Container::Mkv => a.extend(["-f".into(), "matroska".into()]),
        Container::Mp4 => a.extend([
            "-video_track_timescale".into(),
            sps.to_string(),
            "-f".into(),
            "mp4".into(),
        ]),
    }
    let mut args: Vec<OsString> = a.into_iter().map(OsString::from).collect();
    args.push(job.output.into());
    args
}

/// Lines FFmpeg always prints for our streams, which say nothing about this export.
/// SVT-AV1 prints its banner whatever the log level is, and AV1 itself has no way
/// to say centre chroma, so that tag is only held by the container.
fn is_noise(line: &str) -> bool {
    line.contains("Guessed Channel Layout")
        || line.starts_with("Svt[info]")
        || line.contains("chroma sample location center is unsupported")
}

/// A running FFmpeg, with its output being read on another thread so it can't
/// block on a full pipe while we're blocked writing to it.
pub(crate) struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: JoinHandle<Vec<String>>,
}

impl Process {
    pub fn spawn(program: &Path, args: &[OsString]) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| Error::FfmpegStart {
                program: program.to_path_buf(),
                source,
            })?;
        let stdin = child.stdin.take();
        let stderr = child.stderr.take().expect("stderr was piped");
        let stderr = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in BufReader::new(stderr).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line).trim_end().to_string();
                if line.is_empty() || is_noise(&line) {
                    continue;
                }
                if lines.len() == KEPT_LINES {
                    lines.remove(0);
                }
                lines.push(line);
            }
            lines
        });
        Ok(Process {
            child,
            stdin,
            stderr,
        })
    }

    pub fn take_stdin(&mut self) -> ChildStdin {
        self.stdin.take().expect("stdin already taken")
    }

    /// Wait for FFmpeg to finish, returning what it printed. Its stdin must have
    /// been closed by now, or this waits forever.
    pub fn finish(mut self) -> Result<Vec<String>> {
        drop(self.stdin.take());
        let status = self.child.wait().map_err(Error::Io)?;
        let lines = self.stderr.join().unwrap_or_default();
        if status.success() {
            Ok(lines)
        } else {
            Err(Error::FfmpegFailed {
                status: status.to_string(),
                output: lines.join("\n"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(job: &Job) -> Vec<String> {
        args(job)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn value_of<'a>(a: &'a [String], flag: &str) -> Option<&'a str> {
        let at = a.iter().position(|x| x == flag)?;
        Some(&a[at + 1])
    }

    #[test]
    fn time_base_follows_the_disc_speed() {
        for (sps, want) in [(75, "1:75"), (150, "1:150")] {
            let a = strings(&Job {
                preset: Preset::Archive,
                sectors_per_second: sps,
                pix_fmt: "yuv420p10le",
                container: Container::Mkv,
                output: Path::new("o.mkv"),
            });
            assert_eq!(value_of(&a, "-enc_time_base:v"), Some(want));
        }
    }

    #[test]
    fn input_range_comes_before_the_input_and_output_comes_last() {
        let a = strings(&Job {
            preset: Preset::H264,
            sectors_per_second: 150,
            pix_fmt: "yuv420p",
            container: Container::Mp4,
            output: Path::new("o.mp4"),
        });
        let input = a.iter().position(|x| x == "-i").unwrap();
        let range = a.iter().position(|x| x == "-color_range").unwrap();
        assert!(range < input);
        assert_eq!(a.iter().filter(|x| *x == "-color_range").count(), 2);
        assert_eq!(a.last().unwrap(), "o.mp4");
        assert_eq!(value_of(&a, "-video_track_timescale"), Some("150"));
        assert_eq!(value_of(&a, "-pix_fmt"), Some("yuv420p"));
    }

    #[test]
    fn explicit_ffmpeg_wins() {
        assert_eq!(
            locate(Some(Path::new("custom/ffmpeg"))),
            Path::new("custom/ffmpeg")
        );
    }
}
