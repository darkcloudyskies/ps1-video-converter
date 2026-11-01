//! The two inverse DCTs.
//!
//! Both compute `Cᵀ · M · C` for an 8×8 coefficient matrix `M`, where the rows
//! of `C` are the DCT basis functions. They differ in how exact they are:
//! [`accurate`] uses the 16-bit fixed-point matrix games upload to the MDEC and
//! rounds to whole numbers, while [`hq`] uses exact cosines and keeps fractions.

use std::f64::consts::PI;
use std::sync::OnceLock;

/// The cosine matrix games upload to the MDEC: the orthonormal DCT basis in
/// 16-bit fixed point, one basis function per row.
#[rustfmt::skip]
const SONY_MATRIX: [i64; 64] = [
    23170,  23170,  23170,  23170,  23170,  23170,  23170,  23170,
    32138,  27245,  18204,   6392,  -6393, -18205, -27246, -32139,
    30273,  12539, -12540, -30274, -30274, -12540,  12539,  30273,
    27245,  -6393, -32139, -18205,  18204,  32138,   6392, -27246,
    23170, -23171, -23171,  23170,  23170, -23171, -23171,  23170,
    18204, -32139,   6392,  27245, -27246,  -6393,  32138, -18205,
    12539, -30274,  30273, -12540, -12540,  30273, -30274,  12539,
     6392, -18205,  27245, -32139,  32138, -27246,  18204,  -6393,
];

/// Fixed-point IDCT with Sony's matrix, rounding once at the end.
pub fn accurate(coefs: &[i32; 64]) -> [i32; 64] {
    let mut temp = [0i64; 64];
    for row in 0..8 {
        for col in 0..8 {
            temp[row * 8 + col] = (0..8)
                .map(|i| coefs[i * 8 + col] as i64 * SONY_MATRIX[i * 8 + row])
                .sum();
        }
    }
    let mut out = [0i32; 64];
    for row in 0..8 {
        for col in 0..8 {
            let sum: i64 = (0..8)
                .map(|i| SONY_MATRIX[i * 8 + col] * temp[row * 8 + i])
                .sum();
            out[row * 8 + col] = ((sum + (1 << 31)) >> 32) as i32;
        }
    }
    out
}

fn exact_matrix() -> &'static [f64; 64] {
    static MATRIX: OnceLock<[f64; 64]> = OnceLock::new();
    MATRIX.get_or_init(|| {
        let mut m = [0.0; 64];
        for u in 0..8 {
            let scale = if u == 0 { 8f64.sqrt().recip() } else { 0.5 };
            for x in 0..8 {
                m[u * 8 + x] = scale * (((2 * x + 1) * u) as f64 * PI / 16.0).cos();
            }
        }
        m
    })
}

/// Double-precision IDCT with exact cosines and no intermediate rounding.
pub fn hq(coefs: &[f64; 64]) -> [f64; 64] {
    let c = exact_matrix();
    let mut temp = [0.0; 64];
    for row in 0..8 {
        for col in 0..8 {
            temp[row * 8 + col] = (0..8).map(|i| coefs[i * 8 + col] * c[i * 8 + row]).sum();
        }
    }
    let mut out = [0.0; 64];
    for row in 0..8 {
        for col in 0..8 {
            out[row * 8 + col] = (0..8).map(|i| c[i * 8 + col] * temp[row * 8 + i]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The textbook IDCT formula, evaluated directly.
    fn reference(coefs: &[f64; 64]) -> [f64; 64] {
        let cu = |u: usize| {
            if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        let mut out = [0.0; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut sum = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        sum += cu(u)
                            * cu(v)
                            * coefs[v * 8 + u]
                            * (((2 * x + 1) * u) as f64 * PI / 16.0).cos()
                            * (((2 * y + 1) * v) as f64 * PI / 16.0).cos();
                    }
                }
                out[y * 8 + x] = sum / 4.0;
            }
        }
        out
    }

    /// A block with energy in every coefficient, reproducibly.
    fn busy_block() -> [i32; 64] {
        let mut b = [0; 64];
        let mut v: i32 = 12345;
        for c in &mut b {
            v = v.wrapping_mul(1103515245).wrapping_add(12345);
            *c = (v >> 16) % 300;
        }
        b[0] = 400;
        b
    }

    #[test]
    fn hq_matches_the_textbook_formula() {
        let coefs = busy_block().map(|c| c as f64);
        let got = hq(&coefs);
        let want = reference(&coefs);
        for i in 0..64 {
            assert!(
                (got[i] - want[i]).abs() < 1e-9,
                "{i}: {} {}",
                got[i],
                want[i]
            );
        }
    }

    #[test]
    fn accurate_stays_within_one_of_hq() {
        let coefs = busy_block();
        let exact = hq(&coefs.map(|c| c as f64));
        let fixed = accurate(&coefs);
        for i in 0..64 {
            assert!(
                (fixed[i] as f64 - exact[i]).abs() <= 1.0,
                "{i}: {} {}",
                fixed[i],
                exact[i]
            );
        }
    }

    #[test]
    fn dc_only_gives_a_flat_block() {
        let mut coefs = [0; 64];
        coefs[0] = 800;
        assert_eq!(accurate(&coefs), [100; 64]);
        let mut coefs = [0.0; 64];
        coefs[0] = 800.0;
        for v in hq(&coefs) {
            assert!((v - 100.0).abs() < 1e-9);
        }
    }

    #[test]
    fn accurate_matches_jpsxdec_vector() {
        // Output of jPSXdec's PsxMdecIDCT_int for `busy_block`, so a change in
        // rounding or orientation fails here without needing a disc.
        let want: [i32; 64] = include!("../tests/accurate_idct_vector.in");
        assert_eq!(accurate(&busy_block()), want);
    }
}
