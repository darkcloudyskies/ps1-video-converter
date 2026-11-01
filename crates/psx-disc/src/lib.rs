//! Low-level access to PlayStation 1 disc images.
//!
//! PS1 FMV lives in raw 2352-byte CD-ROM XA sectors: video usually in Mode 2
//! Form 1 sectors and XA ADPCM audio in Mode 2 Form 2 sectors. A plain 2048-byte
//! `.iso` throws away the subheaders and all Form 2 data, so this crate always
//! works on raw sectors (BIN/CUE).

pub mod cue;
pub mod edc;
pub mod error;
pub mod iso9660;
pub mod sector;
pub mod source;

pub use error::{Error, Result};
pub use sector::{RawSector, SectorKind, SubHeader, SubMode, RAW_SECTOR_SIZE};
pub use source::{BinCueDisc, SectorSource};
