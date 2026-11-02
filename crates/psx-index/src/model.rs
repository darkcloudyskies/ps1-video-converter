//! The serialisable disc index (`*.psxidx.json`).

use crate::audio::XaFormat;
use psx_disc::iso9660::IsoFile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const INDEX_FORMAT_VERSION: u32 = 1;

/// Where a disc's index is kept by default: `<name>.psxidx.json` beside it.
pub fn default_index_path(disc: &Path) -> PathBuf {
    let stem = disc.file_stem().unwrap_or_default().to_string_lossy();
    disc.with_file_name(format!("{stem}.psxidx.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscIndex {
    pub format_version: u32,
    /// Path of the scanned `.cue`/`.bin` as given on the command line.
    pub source: String,
    pub sector_count: u32,
    pub scanned_range: (u32, u32),
    pub stats: SectorStats,
    pub files: Vec<IsoFile>,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SectorStats {
    pub mode1: u32,
    pub mode2_form1: u32,
    pub mode2_form2: u32,
    pub cdda_or_unknown: u32,
    pub video_sectors: u32,
    pub audio_sectors: u32,
    /// Sectors whose EDC didn't match. Non-zero usually means a bad rip.
    pub edc_errors: u32,
    /// The first few bad sectors, for diagnostics.
    pub edc_error_lbas: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    pub number: u32,
    pub first_lba: u32,
    /// Sector holding the frame's final chunk. A player can display the
    /// frame once this sector has been read, so it drives the timestamp.
    pub last_lba: u32,
    pub chunk_count: u16,
    pub chunks_found: u16,
    pub demux_size: u32,
    pub width: u16,
    pub height: u16,
    pub quant_scale: u16,
    pub version: u16,
}

impl FrameInfo {
    pub fn complete(&self) -> bool {
        self.chunks_found == self.chunk_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timing {
    /// Disc read rate: 75 (1×) or 150 (2×).
    pub sectors_per_second: u32,
    /// `"audio"` if inferred from an interleaved XA stream, else `"assumed"`.
    pub speed_source: String,
    /// Average frame rate over the stream.
    pub fps: f64,
    /// Histogram of sector distances between consecutive frames:
    /// `(distance, occurrences)`, most common first.
    pub frame_sector_deltas: Vec<(u32, u32)>,
}

impl Timing {
    /// True when every frame is the same number of sectors apart.
    pub fn is_constant(&self) -> bool {
        self.frame_sector_deltas.len() <= 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStream {
    pub id: usize,
    pub file: Option<String>,
    pub channel: u8,
    pub first_lba: u32,
    pub last_lba: u32,
    pub width: u16,
    pub height: u16,
    pub variant: String,
    pub frame_count: usize,
    pub incomplete_frames: usize,
    pub timing: Timing,
    /// Ids of audio streams interleaved with this video.
    pub audio: Vec<usize>,
    pub frames: Vec<FrameInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStream {
    pub id: usize,
    pub file: Option<String>,
    /// Subheader file number and channel.
    pub xa_file: u8,
    pub xa_channel: u8,
    pub first_lba: u32,
    pub last_lba: u32,
    pub sector_count: u32,
    pub format: XaFormat,
    /// Most common distance between consecutive audio sectors.
    pub stride: Option<u32>,
    /// Playback length in seconds.
    pub duration_secs: f64,
}
