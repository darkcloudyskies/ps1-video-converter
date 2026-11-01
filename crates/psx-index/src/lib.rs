//! Disc indexer: scans every sector of a PS1 disc's data track and groups
//! STR video sectors and XA audio sectors into streams, recording the sector
//! position of every frame so exports can use exact timestamps.

pub mod audio;
pub mod model;
pub mod scan;
pub mod video;

pub use model::*;
pub use scan::{scan, ScanOptions};
