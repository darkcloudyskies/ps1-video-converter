//! Turning coefficients into YCbCr planes.

use crate::bitstream::{decode_coefficients, macroblocks, Block, BLOCKS_PER_MACROBLOCK};
use crate::idct;
use crate::tables::QUANT;
use crate::Result;

/// Which IDCT and dequantization to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Idct {
    /// Fixed-point math with whole-number results, as the console's MDEC does.
    Accurate,
    /// Double precision with no rounding along the way.
    #[default]
    Hq,
}

/// A decoded frame in YCbCr 4:2:0.
///
/// Samples are centred on zero with a nominal range of -128 to 127, and aren't
/// clamped: values outside that range are real overshoot from the IDCT. Planes
/// cover whole macroblocks, so they may be larger than the visible frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Planes {
    pub width: u16,
    pub height: u16,
    /// Luma samples per row: `width` rounded up to a multiple of 16.
    pub coded_width: usize,
    pub coded_height: usize,
    pub y: Vec<f64>,
    pub cb: Vec<f64>,
    pub cr: Vec<f64>,
}

impl Planes {
    /// Chroma samples per row.
    pub fn chroma_width(&self) -> usize {
        self.coded_width / 2
    }

    /// The visible frame as 8-bit RGB, for quick visual checks only. Chroma is
    /// repeated rather than interpolated.
    pub fn to_rgb8(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let to_u8 = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        let mut rgb = Vec::with_capacity(w * h * 3);
        for row in 0..h {
            for col in 0..w {
                let c = row / 2 * self.chroma_width() + col / 2;
                let y = self.y[row * self.coded_width + col] + 128.0;
                let (cb, cr) = (self.cb[c], self.cr[c]);
                rgb.push(to_u8(y + 1.402 * cr));
                rgb.push(to_u8(y - 0.3437 * cb - 0.7143 * cr));
                rgb.push(to_u8(y + 1.772 * cb));
            }
        }
        rgb
    }
}

fn block_samples(block: &Block, method: Idct) -> [f64; 64] {
    let q = block.qscale as i32;
    match method {
        Idct::Accurate => {
            let mut m = [0i32; 64];
            m[0] = block.coefs[0] as i32 * QUANT[0];
            for i in 1..64 {
                let c = block.coefs[i] as i32;
                m[i] = (c * QUANT[i] * q + 4) >> 3;
            }
            idct::accurate(&m).map(|v| v as f64)
        }
        Idct::Hq => {
            let mut m = [0.0; 64];
            m[0] = (block.coefs[0] as i32 * QUANT[0]) as f64;
            for i in 1..64 {
                m[i] = (block.coefs[i] as i32 * QUANT[i] * q) as f64 / 8.0;
            }
            idct::hq(&m)
        }
    }
}

fn copy_block(samples: &[f64; 64], plane: &mut [f64], stride: usize, x: usize, y: usize) {
    for (row, line) in samples.chunks_exact(8).enumerate() {
        let at = (y + row) * stride + x;
        plane[at..at + 8].copy_from_slice(line);
    }
}

/// Run the IDCT over a frame's blocks, as returned by [`decode_coefficients`].
pub fn reconstruct(blocks: &[Block], width: u16, height: u16, method: Idct) -> Planes {
    let (mb_w, mb_h) = macroblocks(width, height);
    assert_eq!(blocks.len(), mb_w * mb_h * BLOCKS_PER_MACROBLOCK);
    let (luma_w, luma_h) = (mb_w * 16, mb_h * 16);
    let (chroma_w, chroma_h) = (mb_w * 8, mb_h * 8);
    let mut p = Planes {
        width,
        height,
        coded_width: luma_w,
        coded_height: luma_h,
        y: vec![0.0; luma_w * luma_h],
        cb: vec![0.0; chroma_w * chroma_h],
        cr: vec![0.0; chroma_w * chroma_h],
    };
    for (i, mb) in blocks.chunks_exact(BLOCKS_PER_MACROBLOCK).enumerate() {
        // Macroblocks run down each column in turn.
        let (mx, my) = (i / mb_h, i % mb_h);
        let (cx, cy) = (mx * 8, my * 8);
        copy_block(&block_samples(&mb[0], method), &mut p.cr, chroma_w, cx, cy);
        copy_block(&block_samples(&mb[1], method), &mut p.cb, chroma_w, cx, cy);
        for (n, block) in mb[2..].iter().enumerate() {
            let x = mx * 16 + (n % 2) * 8;
            let y = my * 16 + (n / 2) * 8;
            copy_block(&block_samples(block, method), &mut p.y, luma_w, x, y);
        }
    }
    p
}

/// Decode a demuxed STR frame all the way to planes.
pub fn decode_frame(data: &[u8], width: u16, height: u16, method: Idct) -> Result<Planes> {
    let blocks = decode_coefficients(data, width, height)?;
    Ok(reconstruct(&blocks, width, height, method))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::encode_frame;

    fn flat_block(dc: i16) -> Block {
        let mut coefs = [0; 64];
        coefs[0] = dc;
        Block { qscale: 1, coefs }
    }

    /// Two macroblocks wide and two tall, with each block's DC identifying it.
    /// DC 4 per step: DC × 2 ÷ 8 makes each step exactly one sample value.
    fn numbered_blocks() -> Vec<Block> {
        (0..24).map(|i| flat_block(i as i16 * 4)).collect()
    }

    #[test]
    fn macroblocks_run_down_columns_and_blocks_land_in_place() {
        for method in [Idct::Accurate, Idct::Hq] {
            let p = reconstruct(&numbered_blocks(), 32, 32, method);
            assert_eq!((p.coded_width, p.coded_height), (32, 32));
            let y = |x: usize, row: usize| p.y[row * 32 + x].round();
            // Macroblock 0 is top left: Y1..Y4 = blocks 2..5.
            assert_eq!(
                (y(0, 0), y(8, 0), y(0, 8), y(8, 8)),
                (2.0, 3.0, 4.0, 5.0),
                "{method:?}"
            );
            // Macroblock 1 is below it, not beside it: blocks 8..11.
            assert_eq!(y(0, 16), 8.0);
            assert_eq!(y(8, 24), 11.0);
            // Macroblock 2 starts the second column: blocks 14..17.
            assert_eq!(y(16, 0), 14.0);
            // Chroma: Cr then Cb, same layout at half size.
            assert_eq!(p.cr[0].round(), 0.0);
            assert_eq!(p.cb[0].round(), 1.0);
            assert_eq!(p.cr[8 * 16].round(), 6.0);
            assert_eq!(p.cb[8].round(), 13.0);
        }
    }

    #[test]
    fn dequantization_differs_only_in_rounding() {
        let mut coefs = [0; 64];
        coefs[1] = 1; // 1 × QUANT[1] (16) × qscale (3) ÷ 8 = 6, no rounding
        let exact = Block { qscale: 3, coefs };
        let a = block_samples(&exact, Idct::Accurate);
        let h = block_samples(&exact, Idct::Hq);
        for i in 0..64 {
            assert!((a[i] - h[i]).abs() <= 1.0);
        }
        // 1 × 19 × 1 ÷ 8 = 2.375: accurate rounds to 2, hq keeps the fraction.
        let mut coefs = [0; 64];
        coefs[2] = 1;
        let inexact = Block { qscale: 1, coefs };
        let a = block_samples(&inexact, Idct::Accurate);
        let h = block_samples(&inexact, Idct::Hq);
        let mut m = [0.0; 64];
        m[2] = 2.375;
        assert_eq!(h, idct::hq(&m));
        let mut m = [0; 64];
        m[2] = 2;
        assert_eq!(a, idct::accurate(&m).map(|v| v as f64));
    }

    #[test]
    fn negative_coefficients_round_half_up_when_dequantized() {
        // -1 × 16 × 1 = -16, and (-16 + 4) >> 3 floors -1.5 to -2.
        let mut coefs = [0; 64];
        coefs[1] = -1;
        let b = Block { qscale: 1, coefs };
        let mut m = [0; 64];
        m[1] = -2;
        assert_eq!(
            block_samples(&b, Idct::Accurate),
            idct::accurate(&m).map(|v| v as f64)
        );
    }

    #[test]
    fn rgb_uses_full_range_and_crops_to_the_visible_size() {
        // One macroblock, cropped to 10×6. DC 0 means mid-grey.
        let blocks: Vec<Block> = (0..6).map(|_| flat_block(0)).collect();
        let p = reconstruct(&blocks, 10, 6, Idct::Accurate);
        let rgb = p.to_rgb8();
        assert_eq!(rgb.len(), 10 * 6 * 3);
        assert!(rgb.iter().all(|&v| v == 128));

        // Full-scale luma isn't compressed into a 16..235 range.
        let mut blocks: Vec<Block> = (0..6).map(|_| flat_block(0)).collect();
        for b in &mut blocks[2..] {
            b.coefs[0] = 511;
        }
        let rgb = reconstruct(&blocks, 16, 16, Idct::Accurate).to_rgb8();
        assert!(rgb.iter().all(|&v| v == 255), "{:?}", &rgb[..3]);
    }

    #[test]
    fn decode_frame_runs_the_whole_chain() {
        let data = encode_frame(&numbered_blocks(), 1, 2);
        let p = decode_frame(&data, 32, 32, Idct::Hq).unwrap();
        assert_eq!(p.y[0].round(), 2.0);
        let data = encode_frame(&numbered_blocks(), 1, 3);
        let q = decode_frame(&data, 32, 32, Idct::Hq).unwrap();
        assert_eq!(p, q, "STRv2 and STRv3 hold the same picture");
    }
}
