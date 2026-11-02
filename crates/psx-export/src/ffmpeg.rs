//! Running FFmpeg. Everything that knows about its command line or its process
//! lives here, so the rest of the code doesn't depend on how it's run.

use crate::{AudioCodec, Error, Preset, Result};
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
    /// `None` if the stream has no audio, since FFmpeg warns about unused options.
    pub audio: Option<AudioCodec>,
    pub sectors_per_second: u32,
    /// The frame rate as a fraction, for constant frame rate output.
    pub constant_rate: Option<(u32, u32)>,
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
    // The time base is one sector, or one frame when the rate is constant.
    let (timescale, tick) = job.constant_rate.unwrap_or((job.sectors_per_second, 1));
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
    a.extend(["-enc_time_base:v".into(), format!("{tick}:{timescale}")]);
    // Without setparams, primaries and transfer come out `unknown`. The fps filter
    // starts at zero so the picture still lines up with audio that starts first.
    let fps = job
        .constant_rate
        .map(|(n, d)| format!("fps=fps={n}/{d}:start_time=0:round=near,"))
        .unwrap_or_default();
    a.extend([
        "-vf".into(),
        format!(
            "{fps}setparams=color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m:range=pc"
        ),
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
        // -g: a key frame every 2 s at 15 fps. The encoders' defaults leave 11 to 16 s
        // between them, which makes seeking slow and jumpy.
        Preset::H264 => &["-c:v", "libx264", "-crf", "14", "-g", "30"],
        Preset::Av1 => &[
            "-c:v",
            "libsvtav1",
            "-crf",
            "20",
            "-preset",
            "6",
            "-g",
            "30",
        ],
    };
    a.extend(codec.iter().map(|s| s.to_string()));
    let audio: &[&str] = match job.audio {
        None => &[],
        Some(AudioCodec::Flac) => &["-c:a", "flac"],
        Some(AudioCodec::Opus) => &["-c:a", "libopus", "-b:a", "128k", "-ar", "48000"],
        Some(AudioCodec::Aac) => &["-c:a", "aac", "-b:a", "192k", "-ar", "48000"],
    };
    a.extend(audio.iter().map(|s| s.to_string()));
    match job.container {
        Container::Mkv => a.extend(["-f".into(), "matroska".into()]),
        Container::Mp4 => a.extend([
            "-video_track_timescale".into(),
            timescale.to_string(),
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
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // CREATE_NO_WINDOW: a GUI program has no console, so Windows would open one.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        let mut child = command.spawn().map_err(|source| Error::FfmpegStart {
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

    fn job(output: &str) -> Job<'_> {
        Job {
            preset: Preset::Archive,
            audio: Some(AudioCodec::Flac),
            sectors_per_second: 150,
            constant_rate: None,
            pix_fmt: "yuv420p10le",
            container: Container::Mkv,
            output: Path::new(output),
        }
    }

    #[test]
    fn constant_rate_sets_the_filter_and_time_base() {
        let a = strings(&Job {
            preset: Preset::H264,
            constant_rate: Some((15000, 1001)),
            container: Container::Mp4,
            ..job("o.mp4")
        });
        assert_eq!(value_of(&a, "-enc_time_base:v"), Some("1001:15000"));
        assert_eq!(value_of(&a, "-video_track_timescale"), Some("15000"));
        assert_eq!(value_of(&a, "-fps_mode"), Some("passthrough"));
        assert!(value_of(&a, "-vf")
            .unwrap()
            .starts_with("fps=fps=15000/1001:start_time=0:round=near,setparams="));
    }

    #[test]
    fn variable_rate_has_no_fps_filter() {
        let a = strings(&job("o.mkv"));
        assert!(value_of(&a, "-vf").unwrap().starts_with("setparams="));
    }

    #[test]
    fn lossy_audio_is_resampled_to_48_khz_and_flac_is_not() {
        let a = strings(&job("o.mkv"));
        assert_eq!(value_of(&a, "-c:a"), Some("flac"));
        assert_eq!(value_of(&a, "-ar"), None);
        for (audio, name) in [(AudioCodec::Opus, "libopus"), (AudioCodec::Aac, "aac")] {
            let a = strings(&Job {
                audio: Some(audio),
                ..job("o.mkv")
            });
            assert_eq!(value_of(&a, "-c:a"), Some(name));
            assert_eq!(value_of(&a, "-ar"), Some("48000"));
        }
    }

    #[test]
    fn lossy_presets_set_a_short_key_frame_interval() {
        for preset in [Preset::H264, Preset::Av1] {
            let a = strings(&Job {
                preset,
                ..job("o.mkv")
            });
            assert_eq!(value_of(&a, "-g"), Some("30"));
        }
    }

    #[test]
    fn no_audio_means_no_audio_options() {
        let a = strings(&Job {
            audio: None,
            ..job("o.mkv")
        });
        for flag in ["-c:a", "-b:a", "-ar"] {
            assert_eq!(value_of(&a, flag), None);
        }
    }

    fn value_of<'a>(a: &'a [String], flag: &str) -> Option<&'a str> {
        let at = a.iter().position(|x| x == flag)?;
        Some(&a[at + 1])
    }

    #[test]
    fn time_base_follows_the_disc_speed() {
        for (sps, want) in [(75, "1:75"), (150, "1:150")] {
            let a = strings(&Job {
                sectors_per_second: sps,
                ..job("o.mkv")
            });
            assert_eq!(value_of(&a, "-enc_time_base:v"), Some(want));
        }
    }

    #[test]
    fn input_range_comes_before_the_input_and_output_comes_last() {
        let a = strings(&Job {
            preset: Preset::H264,
            pix_fmt: "yuv420p",
            container: Container::Mp4,
            ..job("o.mp4")
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
