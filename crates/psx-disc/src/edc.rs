//! CD-ROM Error Detection Code (the 32-bit CRC stored in each data sector).

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut e = i as u32;
        let mut b = 0;
        while b < 8 {
            e = (e >> 1) ^ if e & 1 != 0 { 0xD801_8001 } else { 0 };
            b += 1;
        }
        table[i] = e;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = make_table();

/// Compute the EDC over `data`.
pub fn compute(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |edc, &b| {
        (edc >> 8) ^ TABLE[((edc ^ b as u32) & 0xFF) as usize]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(compute(&[]), 0);
    }

    fn bitwise(data: &[u8]) -> u32 {
        let mut edc = 0u32;
        for &b in data {
            edc ^= b as u32;
            for _ in 0..8 {
                edc = (edc >> 1) ^ if edc & 1 != 0 { 0xD801_8001 } else { 0 };
            }
        }
        edc
    }

    #[test]
    fn table_matches_bitwise() {
        let data: Vec<u8> = (0..2056u32).map(|i| (i * 7 + 3) as u8).collect();
        assert_eq!(compute(&data), bitwise(&data));
    }

    #[test]
    fn appending_edc_gives_zero_residue() {
        let mut data: Vec<u8> = (0..100u8).collect();
        let edc = compute(&data);
        data.extend_from_slice(&edc.to_le_bytes());
        assert_eq!(compute(&data), 0);
    }
}
