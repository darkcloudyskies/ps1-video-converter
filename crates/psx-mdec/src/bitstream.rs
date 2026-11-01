//! STRv1, STRv2 and STRv3 bitstream decoding, down to quantized coefficients.
//!
//! A demuxed frame starts with an 8-byte header. Then come the blocks: each
//! macroblock holds Cr, Cb, Y1, Y2, Y3, Y4, and each block is a DC value, some
//! AC run/level codes and an end-of-block code. STRv1 and STRv2 are the same
//! apart from the version number, and store DC as plain 10-bit values. STRv3
//! uses MPEG-1 style differentials from the previous block of the same kind.

use crate::bits::{BitReader, OutOfBits};
use crate::tables::{
    ac_table, match_dc_code, AcSymbol, AC_PEEK_BITS, DC_CHROMA_CODES, DC_LUMA_CODES, ZIGZAG,
};
use crate::{Error, Result};

pub const HEADER_SIZE: usize = 8;
pub const BLOCKS_PER_MACROBLOCK: usize = 6;
const MAGIC_3800: u16 = 0x3800;

/// Quantized coefficients of one 8×8 block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub qscale: u8,
    /// In raster order. `coefs[0]` is DC.
    pub coefs: [i16; 64],
}

/// The bitstream's own header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Half the frame's MDEC code count, rounded up.
    pub mdec_words: u16,
    pub qscale: u8,
    pub version: u16,
}

impl FrameHeader {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_SIZE {
            return Err(Error::Unsupported(format!(
                "only {} bytes of frame data",
                data.len()
            )));
        }
        let word = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
        let (magic, qscale, version) = (word(2), word(4), word(6));
        if magic != MAGIC_3800 || !(1..=63).contains(&qscale) {
            return Err(Error::Unsupported(format!(
                "bad header (magic {magic:#06x}, qscale {qscale})"
            )));
        }
        if !(1..=3).contains(&version) {
            return Err(Error::Unsupported(format!("STR version {version}")));
        }
        Ok(FrameHeader {
            mdec_words: word(0),
            qscale: qscale as u8,
            version,
        })
    }
}

/// Number of macroblocks across and down a frame of this size.
pub fn macroblocks(width: u16, height: u16) -> (usize, usize) {
    (
        (width as usize).div_ceil(16),
        (height as usize).div_ceil(16),
    )
}

/// DC predictors for STRv3. All four Y blocks share one, and none reset between
/// macroblocks.
#[derive(Default)]
struct DcPredictors {
    cr: i32,
    cb: i32,
    y: i32,
}

fn read_dc_v3(
    r: &mut BitReader,
    block_kind: usize,
    pred: &mut DcPredictors,
) -> std::result::Result<i32, String> {
    let (table, peek, last): (&[_], u32, &mut i32) = match block_kind {
        0 => (&DC_CHROMA_CODES, 8, &mut pred.cr),
        1 => (&DC_CHROMA_CODES, 8, &mut pred.cb),
        _ => (&DC_LUMA_CODES, 7, &mut pred.y),
    };
    let peeked = r.peek(peek);
    let (len, size) =
        match_dc_code(table, peeked, peek).ok_or_else(|| format!("bad DC code {peeked:#b}"))?;
    r.skip(len).map_err(|_| "out of data".to_string())?;
    let diff = if size == 0 {
        0
    } else {
        let v = r.read(size).map_err(|_| "out of data".to_string())? as i32;
        // MPEG-1: a clear top bit means negative.
        if v & (1 << (size - 1)) == 0 {
            v - ((1 << size) - 1)
        } else {
            v
        }
    };
    // STRv3 keeps 8 bits of DC precision; the MDEC expects 10.
    *last += diff * 4;
    if !(-512..=511).contains(last) {
        return Err(format!("DC {last} out of range"));
    }
    Ok(*last)
}

/// Decode all a frame's blocks. They come back in stream order: column by
/// column of macroblocks, each as Cr, Cb, Y1, Y2, Y3, Y4.
pub fn decode_coefficients(data: &[u8], width: u16, height: u16) -> Result<Vec<Block>> {
    let header = FrameHeader::parse(data)?;
    let (mb_w, mb_h) = macroblocks(width, height);
    let total = mb_w * mb_h * BLOCKS_PER_MACROBLOCK;

    let mut r = BitReader::new(&data[HEADER_SIZE..]);
    let table = ac_table();
    let mut pred = DcPredictors::default();
    let mut blocks = Vec::with_capacity(total);

    for block in 0..total {
        let out_of_data = |_: OutOfBits| Error::EndOfData { block };
        let corrupt = |detail: String| Error::Corrupt { block, detail };

        let mut coefs = [0i16; 64];
        coefs[0] = if header.version == 3 {
            read_dc_v3(&mut r, block % BLOCKS_PER_MACROBLOCK, &mut pred).map_err(corrupt)? as i16
        } else {
            r.read_signed(10).map_err(out_of_data)? as i16
        };

        let mut pos = 0usize;
        loop {
            let entry = table[r.peek(AC_PEEK_BITS) as usize];
            let (run, level) = match entry.symbol {
                AcSymbol::Invalid => {
                    return Err(corrupt(format!(
                        "unknown AC code {:#019b}",
                        r.peek(AC_PEEK_BITS)
                    )))
                }
                AcSymbol::EndOfBlock => {
                    r.skip(entry.bits).map_err(out_of_data)?;
                    break;
                }
                AcSymbol::Escape => {
                    r.skip(entry.bits).map_err(out_of_data)?;
                    let run = r.read(6).map_err(out_of_data)? as usize;
                    (run, r.read_signed(10).map_err(out_of_data)? as i16)
                }
                AcSymbol::Coefficient { run, level } => {
                    r.skip(entry.bits).map_err(out_of_data)?;
                    (run as usize, level)
                }
            };
            pos += run + 1;
            if pos >= 64 {
                return Err(corrupt(format!("coefficient position {pos}")));
            }
            coefs[ZIGZAG[pos] as usize] = level;
        }
        blocks.push(Block {
            qscale: header.qscale,
            coefs,
        });
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{encode_frame, sample_blocks};

    #[test]
    fn roundtrip_every_version() {
        let (w, h) = (48, 32);
        for version in [1, 2, 3] {
            let blocks = sample_blocks(w, h, 5);
            let data = encode_frame(&blocks, 5, version);
            assert_eq!(FrameHeader::parse(&data).unwrap().version, version);
            assert_eq!(decode_coefficients(&data, w, h).unwrap(), blocks);
        }
    }

    #[test]
    fn dimensions_round_up_to_macroblocks() {
        assert_eq!(macroblocks(320, 240), (20, 15));
        assert_eq!(macroblocks(321, 1), (21, 1));
    }

    #[test]
    fn rejects_other_versions_and_bad_headers() {
        let mut data = encode_frame(&sample_blocks(16, 16, 1), 1, 2);
        data[6] = 4;
        assert!(matches!(
            decode_coefficients(&data, 16, 16),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            decode_coefficients(&[0; 4], 16, 16),
            Err(Error::Unsupported(_))
        ));
        let mut bad_magic = encode_frame(&sample_blocks(16, 16, 1), 1, 2);
        bad_magic[3] = 0;
        assert!(matches!(
            decode_coefficients(&bad_magic, 16, 16),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn truncated_frame_reports_where_it_stopped() {
        let data = encode_frame(&sample_blocks(16, 16, 1), 1, 2);
        let got = decode_coefficients(&data[..HEADER_SIZE + 2], 16, 16);
        assert!(matches!(got, Err(Error::EndOfData { .. })), "{got:?}");
    }

    #[test]
    fn escape_with_a_zero_level_still_moves_along() {
        // FF7's STRv1 frames contain these, and the console accepts them.
        let mut words = crate::testutil::BitWriter::default();
        for block in 0..BLOCKS_PER_MACROBLOCK {
            words.put(0, 10);
            if block == 0 {
                words.put_str(crate::tables::ESCAPE_CODE);
                words.put(0, 6);
                words.put(0, 10);
                words.put_str(crate::tables::ESCAPE_CODE);
                words.put(0, 6);
                words.put(5, 10);
            }
            words.put_str(crate::tables::EOB_CODE);
        }
        let mut data = vec![0, 0, 0x00, 0x38, 1, 0, 1, 0];
        data.extend(words.finish());
        let blocks = decode_coefficients(&data, 16, 16).unwrap();
        assert_eq!(blocks[0].coefs[ZIGZAG[1] as usize], 0);
        assert_eq!(blocks[0].coefs[ZIGZAG[2] as usize], 5);
    }

    #[test]
    fn run_past_the_end_of_a_block_is_corrupt() {
        // DC, then an escape with a run of 63 twice.
        let mut words = crate::testutil::BitWriter::default();
        words.put(0, 10);
        for _ in 0..2 {
            words.put_str("000001");
            words.put(63, 6);
            words.put(1, 10);
        }
        let mut data = vec![0, 0, 0x00, 0x38, 1, 0, 2, 0];
        data.extend(words.finish());
        let got = decode_coefficients(&data, 16, 16);
        assert!(
            matches!(got, Err(Error::Corrupt { block: 0, .. })),
            "{got:?}"
        );
    }
}
