//! Constant tables: Sony's MDEC matrices and the MPEG-1 variable-length codes.

use std::sync::OnceLock;

/// The quantization matrix every PS1 game uploads to the MDEC, in raster order.
pub const QUANT: [i32; 64] = [
    2, 16, 19, 22, 26, 27, 29, 34, //
    16, 16, 22, 24, 27, 29, 34, 37, //
    19, 22, 26, 27, 29, 34, 34, 38, //
    22, 22, 26, 27, 29, 34, 37, 40, //
    22, 26, 27, 29, 32, 35, 40, 48, //
    26, 27, 29, 32, 35, 40, 48, 58, //
    26, 27, 29, 34, 38, 46, 56, 69, //
    27, 29, 35, 38, 46, 56, 69, 83,
];

/// Raster position of each coefficient in zig-zag order.
pub const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub const EOB_CODE: &str = "10";
pub const ESCAPE_CODE: &str = "000001";

/// AC codes as `(bits, run, level)`, from ISO 11172-2 table 2-B.5. Each is
/// followed by a sign bit (1 = negative). PS1 uses `11s` for run 0 level 1
/// everywhere, where MPEG-1 has a shorter form for a block's first coefficient.
#[rustfmt::skip]
pub const AC_CODES: [(&str, u8, i16); 111] = [
    ("11", 0, 1),
    ("011", 1, 1),
    ("0100", 0, 2), ("0101", 2, 1),
    ("00101", 0, 3), ("00110", 4, 1), ("00111", 3, 1),
    ("000100", 7, 1), ("000101", 6, 1), ("000110", 1, 2), ("000111", 5, 1),
    ("0000100", 2, 2), ("0000101", 9, 1), ("0000110", 0, 4), ("0000111", 8, 1),
    ("00100000", 13, 1), ("00100001", 0, 6), ("00100010", 12, 1), ("00100011", 11, 1),
    ("00100100", 3, 2), ("00100101", 1, 3), ("00100110", 0, 5), ("00100111", 10, 1),
    ("0000001000", 16, 1), ("0000001001", 5, 2), ("0000001010", 0, 7), ("0000001011", 2, 3),
    ("0000001100", 1, 4), ("0000001101", 15, 1), ("0000001110", 14, 1), ("0000001111", 4, 2),
    ("000000010000", 0, 11), ("000000010001", 8, 2), ("000000010010", 4, 3), ("000000010011", 0, 10),
    ("000000010100", 2, 4), ("000000010101", 7, 2), ("000000010110", 21, 1), ("000000010111", 20, 1),
    ("000000011000", 0, 9), ("000000011001", 19, 1), ("000000011010", 18, 1), ("000000011011", 1, 5),
    ("000000011100", 3, 3), ("000000011101", 0, 8), ("000000011110", 6, 2), ("000000011111", 17, 1),
    ("0000000010000", 10, 2), ("0000000010001", 9, 2), ("0000000010010", 5, 3), ("0000000010011", 3, 4),
    ("0000000010100", 2, 5), ("0000000010101", 1, 7), ("0000000010110", 1, 6), ("0000000010111", 0, 15),
    ("0000000011000", 0, 14), ("0000000011001", 0, 13), ("0000000011010", 0, 12), ("0000000011011", 26, 1),
    ("0000000011100", 25, 1), ("0000000011101", 24, 1), ("0000000011110", 23, 1), ("0000000011111", 22, 1),
    ("00000000010000", 0, 31), ("00000000010001", 0, 30), ("00000000010010", 0, 29), ("00000000010011", 0, 28),
    ("00000000010100", 0, 27), ("00000000010101", 0, 26), ("00000000010110", 0, 25), ("00000000010111", 0, 24),
    ("00000000011000", 0, 23), ("00000000011001", 0, 22), ("00000000011010", 0, 21), ("00000000011011", 0, 20),
    ("00000000011100", 0, 19), ("00000000011101", 0, 18), ("00000000011110", 0, 17), ("00000000011111", 0, 16),
    ("000000000010000", 0, 40), ("000000000010001", 0, 39), ("000000000010010", 0, 38), ("000000000010011", 0, 37),
    ("000000000010100", 0, 36), ("000000000010101", 0, 35), ("000000000010110", 0, 34), ("000000000010111", 0, 33),
    ("000000000011000", 0, 32), ("000000000011001", 1, 14), ("000000000011010", 1, 13), ("000000000011011", 1, 12),
    ("000000000011100", 1, 11), ("000000000011101", 1, 10), ("000000000011110", 1, 9), ("000000000011111", 1, 8),
    ("0000000000010000", 1, 18), ("0000000000010001", 1, 17), ("0000000000010010", 1, 16), ("0000000000010011", 1, 15),
    ("0000000000010100", 6, 3), ("0000000000010101", 16, 2), ("0000000000010110", 15, 2), ("0000000000010111", 14, 2),
    ("0000000000011000", 13, 2), ("0000000000011001", 12, 2), ("0000000000011010", 11, 2), ("0000000000011011", 31, 1),
    ("0000000000011100", 30, 1), ("0000000000011101", 29, 1), ("0000000000011110", 28, 1), ("0000000000011111", 27, 1),
];

/// STRv3 luma DC codes as `(bits, size of the differential that follows)`,
/// from ISO 11172-2 table 2-D.12.
pub const DC_LUMA_CODES: [(&str, u32); 9] = [
    ("00", 1),
    ("01", 2),
    ("100", 0),
    ("101", 3),
    ("110", 4),
    ("1110", 5),
    ("11110", 6),
    ("111110", 7),
    ("1111110", 8),
];

/// STRv3 chroma DC codes, in the same form as [`DC_LUMA_CODES`].
pub const DC_CHROMA_CODES: [(&str, u32); 9] = [
    ("00", 0),
    ("01", 1),
    ("10", 2),
    ("110", 3),
    ("1110", 4),
    ("11110", 5),
    ("111110", 6),
    ("1111110", 7),
    ("11111110", 8),
];

/// Longest AC code including its sign bit.
pub const AC_PEEK_BITS: u32 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcSymbol {
    Invalid,
    EndOfBlock,
    Escape,
    Coefficient { run: u8, level: i16 },
}

#[derive(Debug, Clone, Copy)]
pub struct AcEntry {
    pub symbol: AcSymbol,
    /// Bits used, including the sign bit.
    pub bits: u32,
}

fn parse_bits(code: &str) -> u32 {
    u32::from_str_radix(code, 2).expect("tables hold binary strings")
}

/// Fill every table slot that starts with the `len`-bit prefix `code`.
fn fill(table: &mut [AcEntry], code: u32, len: u32, symbol: AcSymbol) {
    let free = AC_PEEK_BITS - len;
    let first = (code << free) as usize;
    for e in &mut table[first..first + (1 << free)] {
        assert_eq!(e.symbol, AcSymbol::Invalid, "AC codes overlap");
        *e = AcEntry { symbol, bits: len };
    }
}

/// Lookup table indexed by the next [`AC_PEEK_BITS`] bits of the stream.
pub fn ac_table() -> &'static [AcEntry] {
    static TABLE: OnceLock<Vec<AcEntry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let invalid = AcEntry {
            symbol: AcSymbol::Invalid,
            bits: 0,
        };
        let mut t = vec![invalid; 1 << AC_PEEK_BITS];
        fill(
            &mut t,
            parse_bits(EOB_CODE),
            EOB_CODE.len() as u32,
            AcSymbol::EndOfBlock,
        );
        fill(
            &mut t,
            parse_bits(ESCAPE_CODE),
            ESCAPE_CODE.len() as u32,
            AcSymbol::Escape,
        );
        for &(bits, run, level) in &AC_CODES {
            let len = bits.len() as u32 + 1;
            let code = parse_bits(bits) << 1;
            fill(&mut t, code, len, AcSymbol::Coefficient { run, level });
            fill(
                &mut t,
                code | 1,
                len,
                AcSymbol::Coefficient { run, level: -level },
            );
        }
        t
    })
}

/// Match the top of `peeked` (holding `peek_bits` bits) against a DC table.
/// Returns the code length and the size of the differential after it.
pub fn match_dc_code(table: &[(&str, u32)], peeked: u32, peek_bits: u32) -> Option<(u32, u32)> {
    table.iter().find_map(|&(code, size)| {
        let len = code.len() as u32;
        (peeked >> (peek_bits - len) == parse_bits(code)).then_some((len, size))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_visits_every_position_once() {
        let mut seen = [false; 64];
        for &p in &ZIGZAG {
            assert!(!seen[p as usize]);
            seen[p as usize] = true;
        }
    }

    #[test]
    fn ac_table_decodes_known_codes() {
        let t = ac_table();
        let at = |code: u32, len: u32| t[(code << (AC_PEEK_BITS - len)) as usize];

        let e = at(0b110, 3);
        assert_eq!(
            (e.symbol, e.bits),
            (AcSymbol::Coefficient { run: 0, level: 1 }, 3)
        );
        let e = at(0b111, 3);
        assert_eq!(e.symbol, AcSymbol::Coefficient { run: 0, level: -1 });
        assert_eq!(at(0b10, 2).symbol, AcSymbol::EndOfBlock);
        assert_eq!(at(0b000001, 6).symbol, AcSymbol::Escape);
        let e = at(0b0_0000_0000_0011_1111, 17);
        assert_eq!(
            (e.symbol, e.bits),
            (AcSymbol::Coefficient { run: 27, level: -1 }, 17)
        );
        // All zeros isn't a code.
        assert_eq!(t[0].symbol, AcSymbol::Invalid);
    }

    #[test]
    fn dc_codes_match_on_prefix() {
        assert_eq!(
            match_dc_code(&DC_LUMA_CODES, 0b1000000, 7),
            Some((3, 0)),
            "100 is luma zero"
        );
        assert_eq!(match_dc_code(&DC_LUMA_CODES, 0b1111111, 7), None);
        assert_eq!(match_dc_code(&DC_CHROMA_CODES, 0b11111110, 8), Some((8, 8)));
        assert_eq!(match_dc_code(&DC_CHROMA_CODES, 0b00000000, 8), Some((2, 0)));
    }
}
