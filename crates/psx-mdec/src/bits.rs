//! Bit reader for STR bitstreams: 16-bit little-endian words, read from the
//! most significant bit down.

/// The bits ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBits;

pub struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit: 0 }
    }

    fn byte(&self, i: usize) -> u32 {
        // XOR 1 swaps the bytes of each 16-bit word.
        self.data.get(i ^ 1).copied().unwrap_or(0) as u32
    }

    /// The next `n` bits (at most 24), without consuming them. Bits past the
    /// end read as zero: the longest code may look further than the shortest
    /// one at the end of a frame actually uses.
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!((1..=24).contains(&n));
        let i = self.bit / 8;
        let word =
            self.byte(i) << 24 | self.byte(i + 1) << 16 | self.byte(i + 2) << 8 | self.byte(i + 3);
        (word << (self.bit % 8)) >> (32 - n)
    }

    pub fn skip(&mut self, n: u32) -> Result<(), OutOfBits> {
        let end = self.bit + n as usize;
        if end > self.data.len() * 8 {
            return Err(OutOfBits);
        }
        self.bit = end;
        Ok(())
    }

    pub fn read(&mut self, n: u32) -> Result<u32, OutOfBits> {
        let v = self.peek(n);
        self.skip(n)?;
        Ok(v)
    }

    pub fn read_signed(&mut self, n: u32) -> Result<i32, OutOfBits> {
        let v = self.read(n)?;
        let unused = 32 - n;
        Ok(((v << unused) as i32) >> unused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_words_from_the_top_bit() {
        // Words 0xABCD 0x1234, stored little-endian.
        let data = [0xCD, 0xAB, 0x34, 0x12];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read(4), Ok(0xA));
        assert_eq!(r.peek(8), 0xBC);
        assert_eq!(r.read(12), Ok(0xBCD));
        assert_eq!(r.read(16), Ok(0x1234));
        assert_eq!(r.read(1), Err(OutOfBits));
    }

    #[test]
    fn peek_past_the_end_pads_with_zeros() {
        // The word 0x00FF.
        let data = [0xFF, 0x00];
        let mut r = BitReader::new(&data);
        r.skip(12).unwrap();
        assert_eq!(r.peek(8), 0xF0);
    }

    #[test]
    fn signed_values_extend_the_sign() {
        // 10 bits of 1 = -1, then 10 bits 0b0111111111 = 511, then -512.
        let bits: u32 = 0b1111_1111_1101_1111_1111_1000_0000_0000;
        let word_hi = (bits >> 16) as u16;
        let word_lo = bits as u16;
        let mut data = word_hi.to_le_bytes().to_vec();
        data.extend_from_slice(&word_lo.to_le_bytes());
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_signed(10), Ok(-1));
        assert_eq!(r.read_signed(10), Ok(511));
        assert_eq!(r.read_signed(10), Ok(-512));
    }
}
