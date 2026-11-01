//! Minimal CUE sheet parser covering what PS1 rips use: one or more `FILE`
//! entries in BINARY format holding `MODE2/2352`, `MODE1/2352` or `AUDIO`
//! tracks.

use crate::error::{io_err, Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TrackMode {
    Mode1,
    Mode2,
    Audio,
}

#[derive(Debug, Clone, Serialize)]
pub struct CueTrack {
    pub number: u32,
    pub mode: TrackMode,
    /// Index into [`CueSheet::files`].
    pub file_index: usize,
    /// Sector offset of INDEX 01 within its file.
    pub file_sector_offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CueSheet {
    pub files: Vec<PathBuf>,
    pub tracks: Vec<CueTrack>,
}

impl CueSheet {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(io_err(path))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, dir, path)
    }

    /// Parse CUE text. Relative `FILE` paths are resolved against `dir`.
    pub fn parse(text: &str, dir: &Path, cue_path: &Path) -> Result<Self> {
        let err = |line: usize, msg: String| Error::Cue {
            path: cue_path.to_path_buf(),
            line,
            msg,
        };
        let mut files = Vec::new();
        let mut tracks: Vec<CueTrack> = Vec::new();
        let mut pending: Option<(u32, TrackMode)> = None;

        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = raw.trim();
            let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            match cmd.to_ascii_uppercase().as_str() {
                "FILE" => {
                    let (name, kind) = parse_file_line(rest)
                        .ok_or_else(|| err(line_no, format!("malformed FILE line: {line}")))?;
                    if !kind.eq_ignore_ascii_case("BINARY") {
                        return Err(err(line_no, format!("unsupported file type {kind}")));
                    }
                    let p = Path::new(&name);
                    files.push(if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        dir.join(p)
                    });
                }
                "TRACK" => {
                    let mut parts = rest.split_whitespace();
                    let num: u32 = parts
                        .next()
                        .and_then(|n| n.parse().ok())
                        .ok_or_else(|| err(line_no, "bad track number".into()))?;
                    let mode = match parts.next().map(|m| m.to_ascii_uppercase()).as_deref() {
                        Some("MODE2/2352") => TrackMode::Mode2,
                        Some("MODE1/2352") => TrackMode::Mode1,
                        Some("AUDIO") => TrackMode::Audio,
                        Some(m) => {
                            return Err(err(
                                line_no,
                                format!("unsupported track mode {m} (need raw 2352-byte sectors)"),
                            ))
                        }
                        None => return Err(err(line_no, "missing track mode".into())),
                    };
                    if files.is_empty() {
                        return Err(err(line_no, "TRACK before FILE".into()));
                    }
                    pending = Some((num, mode));
                }
                "INDEX" => {
                    let mut parts = rest.split_whitespace();
                    let idx: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(99);
                    let msf = parts
                        .next()
                        .and_then(parse_msf)
                        .ok_or_else(|| err(line_no, format!("bad INDEX time: {line}")))?;
                    if idx == 1 {
                        if let Some((number, mode)) = pending.take() {
                            tracks.push(CueTrack {
                                number,
                                mode,
                                file_index: files.len() - 1,
                                file_sector_offset: msf,
                            });
                        }
                    }
                }
                _ => {} // REM, PREGAP, POSTGAP, CATALOG, ... are irrelevant here
            }
        }
        if tracks.is_empty() {
            return Err(err(0, "no tracks found".into()));
        }
        Ok(CueSheet { files, tracks })
    }
}

fn parse_file_line(rest: &str) -> Option<(String, String)> {
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some((
            stripped[..end].to_string(),
            stripped[end + 1..].trim().to_string(),
        ))
    } else {
        let (name, kind) = rest.rsplit_once(char::is_whitespace)?;
        Some((name.trim().to_string(), kind.trim().to_string()))
    }
}

/// Parse `mm:ss:ff` into a sector count.
fn parse_msf(s: &str) -> Option<u32> {
    let mut it = s.split(':').map(|p| p.parse::<u32>().ok());
    let (m, sec, f) = (it.next()??, it.next()??, it.next()??);
    Some((m * 60 + sec) * 75 + f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_multi_track() {
        let cue = r#"FILE "Parasite Eve (Disc 1).bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 50:00:00
    INDEX 01 50:02:00
"#;
        let s = CueSheet::parse(cue, Path::new("/discs"), Path::new("x.cue")).unwrap();
        assert_eq!(
            s.files,
            vec![PathBuf::from("/discs/Parasite Eve (Disc 1).bin")]
        );
        assert_eq!(s.tracks.len(), 2);
        assert_eq!(s.tracks[0].mode, TrackMode::Mode2);
        assert_eq!(s.tracks[1].file_sector_offset, (50 * 60 + 2) * 75);
    }

    #[test]
    fn multi_file() {
        let cue = "FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\nFILE \"b.bin\" BINARY\nTRACK 02 AUDIO\nINDEX 00 00:00:00\nINDEX 01 00:02:00\n";
        let s = CueSheet::parse(cue, Path::new("d"), Path::new("x.cue")).unwrap();
        assert_eq!(s.files.len(), 2);
        assert_eq!(s.tracks[1].file_index, 1);
        assert_eq!(s.tracks[1].file_sector_offset, 150);
    }

    #[test]
    fn rejects_cooked_iso_tracks() {
        let cue = "FILE a.iso BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n";
        assert!(CueSheet::parse(cue, Path::new("d"), Path::new("x.cue")).is_err());
    }
}
