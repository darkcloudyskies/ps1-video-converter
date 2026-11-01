//! A small STR encoder, so tests can build frames from coefficients they know.
//! It's for tests only: it does no rate control and writes no real picture.

use crate::bitstream::{macroblocks, Block, BLOCKS_PER_MACROBLOCK};
use crate::tables::{AC_CODES, DC_CHROMA_CODES, DC_LUMA_CODES, EOB_CODE, ESCAPE_CODE, ZIGZAG};

/// Writes bits from the top down into 16-bit little-endian words.
#[derive(Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    word: u16,
    used: u32,
}

impl BitWriter {
    pub fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.word = self.word << 1 | ((value >> i) & 1) as u16;
            self.used += 1;
            if self.used == 16 {
                self.bytes.extend_from_slice(&self.word.to_le_bytes());
                self.word = 0;
                self.used = 0;
            }
        }
    }

    pub fn put_str(&mut self, bits: &str) {
        for c in bits.chars() {
            self.put((c == '1') as u32, 1);
        }
    }

    /// Pad out the last word and return the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        while self.used != 0 {
            self.put(0, 1);
        }
        self.bytes
    }
}

fn put_dc_v3(w: &mut BitWriter, table: &[(&str, u32)], dc: i32, last: &mut i32) {
    assert_eq!(dc % 4, 0, "STRv3 stores DC in steps of 4");
    let diff = (dc - *last) / 4;
    *last = dc;
    let size = 32 - diff.unsigned_abs().leading_zeros();
    let (code, _) = table
        .iter()
        .find(|&&(_, s)| s == size)
        .expect("DC differential fits the table");
    w.put_str(code);
    if size > 0 {
        let v = if diff < 0 {
            diff + (1 << size) - 1
        } else {
            diff
        };
        w.put(v as u32, size);
    }
}

fn put_ac(w: &mut BitWriter, run: usize, level: i16) {
    let code = AC_CODES
        .iter()
        .find(|&&(_, r, l)| r as usize == run && l == level.abs());
    match code {
        Some(&(bits, _, _)) => {
            w.put_str(bits);
            w.put((level < 0) as u32, 1);
        }
        None => {
            w.put_str(ESCAPE_CODE);
            w.put(run as u32, 6);
            w.put(level as u16 as u32 & 0x3FF, 10);
        }
    }
}

/// Encode blocks (in stream order) as a demuxed frame of the given STR version.
pub fn encode_frame(blocks: &[Block], qscale: u8, version: u16) -> Vec<u8> {
    let mut w = BitWriter::default();
    let (mut last_cr, mut last_cb, mut last_y) = (0, 0, 0);
    let mut codes: u32 = 0;
    for (i, b) in blocks.iter().enumerate() {
        let dc = b.coefs[0] as i32;
        if version == 3 {
            match i % BLOCKS_PER_MACROBLOCK {
                0 => put_dc_v3(&mut w, &DC_CHROMA_CODES, dc, &mut last_cr),
                1 => put_dc_v3(&mut w, &DC_CHROMA_CODES, dc, &mut last_cb),
                _ => put_dc_v3(&mut w, &DC_LUMA_CODES, dc, &mut last_y),
            }
        } else {
            w.put(dc as u32 & 0x3FF, 10);
        }
        let mut run = 0;
        for &pos in &ZIGZAG[1..] {
            let level = b.coefs[pos as usize];
            if level == 0 {
                run += 1;
            } else {
                put_ac(&mut w, run, level);
                run = 0;
                codes += 1;
            }
        }
        w.put_str(EOB_CODE);
        codes += 2; // DC and end of block
    }
    w.put_str(if version == 3 {
        "1111111111"
    } else {
        "0111111111"
    });

    let mut out = Vec::new();
    out.extend_from_slice(&(codes.div_ceil(2) as u16).to_le_bytes());
    out.extend_from_slice(&0x3800u16.to_le_bytes());
    out.extend_from_slice(&(qscale as u16).to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend(w.finish());
    out
}

/// Blocks with assorted coefficients: short codes, long codes and escapes, with
/// DC values that suit both STR versions.
pub fn sample_blocks(width: u16, height: u16, qscale: u8) -> Vec<Block> {
    let (mb_w, mb_h) = macroblocks(width, height);
    let mut v: i32 = 7;
    (0..mb_w * mb_h * BLOCKS_PER_MACROBLOCK)
        .map(|_| {
            let mut coefs = [0i16; 64];
            v = v.wrapping_mul(1103515245).wrapping_add(12345);
            coefs[0] = ((v >> 16) % 120 * 4) as i16;
            for n in 0..((v >> 8) & 15) as usize {
                v = v.wrapping_mul(1103515245).wrapping_add(12345);
                let level = match n % 3 {
                    0 => (v >> 16) % 3,
                    1 => (v >> 16) % 45,
                    _ => (v >> 16) % 511,
                };
                coefs[ZIGZAG[1 + (v >> 20 & 0x3F) as usize % 63] as usize] = level as i16;
            }
            Block { qscale, coefs }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_fills_words_from_the_top() {
        let mut w = BitWriter::default();
        w.put(0xABCD, 16);
        w.put(0b1, 1);
        assert_eq!(w.finish(), [0xCD, 0xAB, 0x00, 0x80]);
    }
}
