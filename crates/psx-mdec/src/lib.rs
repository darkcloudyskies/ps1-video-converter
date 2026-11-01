//! PS1 video decoding: STR bitstream → MDEC coefficients → IDCT → YCbCr planes.
//!
//! Frames stay in YCbCr 4:2:0 all the way through. RGB only exists for the
//! quick-look conversion in [`Planes::to_rgb8`].

mod bits;
pub mod bitstream;
pub mod frame;
pub mod idct;
pub mod tables;
pub mod testutil;

pub use bitstream::{decode_coefficients, Block, FrameHeader};
pub use frame::{decode_frame, reconstruct, Idct, Planes};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not a frame this decoder supports: {0}")]
    Unsupported(String),
    #[error("frame data ran out at block {block}")]
    EndOfData { block: usize },
    #[error("corrupt frame data at block {block}: {detail}")]
    Corrupt { block: usize, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;
