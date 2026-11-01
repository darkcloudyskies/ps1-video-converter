//! The 32-byte header at the start of every STR video sector's user data.

/// `0x80010160` little-endian: the standard Sony MDEC-stream sector tag.
pub const STR_MAGIC: u32 = 0x8001_0160;
pub const HEADER_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrHeader {
    pub chunk_number: u16,
    pub chunk_count: u16,
    pub frame_number: u32,
    /// Bytes of compressed frame data across all chunks.
    pub demux_size: u32,
    pub width: u16,
    pub height: u16,
    /// Number of 32-bit MDEC words the frame decodes to (half the run-length
    /// code count rounded up), as stored by Sony's encoder.
    pub mdec_code_count: u16,
    pub magic_3800: u16,
    pub quant_scale: u16,
    pub version: u16,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl StrHeader {
    /// Parse and sanity-check a header. Returns `None` for anything that isn't
    /// plausibly a standard STR video sector.
    pub fn parse(user_data: &[u8]) -> Option<Self> {
        if user_data.len() < HEADER_SIZE || u32_at(user_data, 0) != STR_MAGIC {
            return None;
        }
        let h = StrHeader {
            chunk_number: u16_at(user_data, 4),
            chunk_count: u16_at(user_data, 6),
            frame_number: u32_at(user_data, 8),
            demux_size: u32_at(user_data, 12),
            width: u16_at(user_data, 16),
            height: u16_at(user_data, 18),
            mdec_code_count: u16_at(user_data, 20),
            magic_3800: u16_at(user_data, 22),
            quant_scale: u16_at(user_data, 24),
            version: u16_at(user_data, 26),
        };
        let ok = (1..=256).contains(&h.chunk_count)
            && h.chunk_number < h.chunk_count
            && (1..=1024).contains(&h.width)
            && (1..=1024).contains(&h.height);
        ok.then_some(h)
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut b = [0u8; HEADER_SIZE];
        b[0..4].copy_from_slice(&STR_MAGIC.to_le_bytes());
        b[4..6].copy_from_slice(&self.chunk_number.to_le_bytes());
        b[6..8].copy_from_slice(&self.chunk_count.to_le_bytes());
        b[8..12].copy_from_slice(&self.frame_number.to_le_bytes());
        b[12..16].copy_from_slice(&self.demux_size.to_le_bytes());
        b[16..18].copy_from_slice(&self.width.to_le_bytes());
        b[18..20].copy_from_slice(&self.height.to_le_bytes());
        b[20..22].copy_from_slice(&self.mdec_code_count.to_le_bytes());
        b[22..24].copy_from_slice(&self.magic_3800.to_le_bytes());
        b[24..26].copy_from_slice(&self.quant_scale.to_le_bytes());
        b[26..28].copy_from_slice(&self.version.to_le_bytes());
        b
    }

    /// Human-readable bitstream variant name. Only the Sony-standard
    /// versions are identified at this stage; game-specific variants are
    /// recognised once their decoders exist.
    pub fn variant(&self) -> String {
        match self.version {
            1 => "STRv1".into(),
            2 => "STRv2".into(),
            3 => "STRv3".into(),
            v => format!("unknown(version={v:#06x})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_validation() {
        let h = StrHeader {
            chunk_number: 2,
            chunk_count: 6,
            frame_number: 17,
            demux_size: 11000,
            width: 320,
            height: 224,
            mdec_code_count: 5000,
            magic_3800: 0x3800,
            quant_scale: 8,
            version: 2,
        };
        assert_eq!(StrHeader::parse(&h.to_bytes()), Some(h));
        assert_eq!(h.variant(), "STRv2");
        let bad = StrHeader {
            chunk_number: 6,
            ..h
        };
        assert_eq!(StrHeader::parse(&bad.to_bytes()), None);
        assert_eq!(StrHeader::parse(&[0u8; 32]), None);
    }
}
