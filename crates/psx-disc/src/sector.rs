//! Raw 2352-byte CD-ROM sector parsing (Mode 1, Mode 2 XA Form 1/2, CD-DA).

use crate::edc;

pub const RAW_SECTOR_SIZE: usize = 2352;
pub const FORM1_DATA_SIZE: usize = 2048;
pub const FORM2_DATA_SIZE: usize = 2324;

const SYNC: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

/// Bit flags of the CD-ROM XA subheader "submode" byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubMode(pub u8);

impl SubMode {
    pub const END_OF_RECORD: u8 = 0x01;
    pub const VIDEO: u8 = 0x02;
    pub const AUDIO: u8 = 0x04;
    pub const DATA: u8 = 0x08;
    pub const TRIGGER: u8 = 0x10;
    pub const FORM2: u8 = 0x20;
    pub const REAL_TIME: u8 = 0x40;
    pub const END_OF_FILE: u8 = 0x80;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
    pub fn is_form2(self) -> bool {
        self.has(Self::FORM2)
    }
    pub fn is_audio(self) -> bool {
        self.has(Self::AUDIO)
    }
    pub fn is_video(self) -> bool {
        self.has(Self::VIDEO)
    }
    pub fn is_eof(self) -> bool {
        self.has(Self::END_OF_FILE)
    }
}

/// The 4-byte CD-ROM XA subheader (stored twice in every Mode 2 sector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubHeader {
    pub file: u8,
    pub channel: u8,
    pub submode: SubMode,
    pub coding_info: u8,
}

impl SubHeader {
    pub fn from_bytes(b: &[u8]) -> Self {
        SubHeader {
            file: b[0],
            channel: b[1],
            submode: SubMode(b[2]),
            coding_info: b[3],
        }
    }
    pub fn to_bytes(self) -> [u8; 4] {
        [self.file, self.channel, self.submode.0, self.coding_info]
    }
}

/// What kind of sector this is, as far as the raw layout is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorKind {
    /// No sync pattern: CD-DA audio or garbage.
    Cdda,
    Mode1,
    Mode2Form1,
    Mode2Form2,
    /// Sync present but an unexpected mode byte.
    Unknown,
}

/// One raw 2352-byte sector.
#[derive(Clone)]
pub struct RawSector {
    pub lba: u32,
    pub bytes: Box<[u8; RAW_SECTOR_SIZE]>,
}

impl std::fmt::Debug for RawSector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawSector")
            .field("lba", &self.lba)
            .field("kind", &self.kind())
            .field("subheader", &self.subheader())
            .finish()
    }
}

impl RawSector {
    pub fn new(lba: u32, bytes: &[u8]) -> Self {
        let mut b = Box::new([0u8; RAW_SECTOR_SIZE]);
        b.copy_from_slice(&bytes[..RAW_SECTOR_SIZE]);
        RawSector { lba, bytes: b }
    }

    pub fn has_sync(&self) -> bool {
        self.bytes[..12] == SYNC
    }

    pub fn mode(&self) -> u8 {
        self.bytes[15]
    }

    /// BCD minute/second/frame address from the sector header.
    pub fn header_msf(&self) -> (u8, u8, u8) {
        (
            bcd(self.bytes[12]),
            bcd(self.bytes[13]),
            bcd(self.bytes[14]),
        )
    }

    pub fn kind(&self) -> SectorKind {
        if !self.has_sync() {
            return SectorKind::Cdda;
        }
        match self.mode() {
            1 => SectorKind::Mode1,
            2 => {
                if self.subheader_raw().submode.is_form2() {
                    SectorKind::Mode2Form2
                } else {
                    SectorKind::Mode2Form1
                }
            }
            _ => SectorKind::Unknown,
        }
    }

    fn subheader_raw(&self) -> SubHeader {
        // Both copies are covered by the EDC, so it can't arbitrate between
        // them. Use the first copy, which is what the drive firmware does.
        SubHeader::from_bytes(&self.bytes[16..20])
    }

    /// The XA subheader for Mode 2 sectors.
    pub fn subheader(&self) -> Option<SubHeader> {
        (self.has_sync() && self.mode() == 2).then(|| self.subheader_raw())
    }

    /// The user data area: 2048 bytes for Mode 1 / Form 1, 2324 for Form 2,
    /// the whole 2352 bytes for CD-DA.
    pub fn user_data(&self) -> &[u8] {
        match self.kind() {
            SectorKind::Mode1 => &self.bytes[16..16 + FORM1_DATA_SIZE],
            SectorKind::Mode2Form1 => &self.bytes[24..24 + FORM1_DATA_SIZE],
            SectorKind::Mode2Form2 => &self.bytes[24..24 + FORM2_DATA_SIZE],
            SectorKind::Cdda | SectorKind::Unknown => &self.bytes[..],
        }
    }

    fn edc_ok_for(&self, form2: bool) -> Option<bool> {
        let (range, at) = if form2 {
            (16..2348, 2348)
        } else {
            (16..2072, 2072)
        };
        let stored = u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap());
        if form2 && stored == 0 {
            // Form 2 EDC is optional; zero means "not present".
            return None;
        }
        Some(edc::compute(&self.bytes[range]) == stored)
    }

    /// Check the sector's EDC. `None` means the sector carries no EDC
    /// (CD-DA, or Form 2 with the EDC left as zero).
    pub fn edc_ok(&self) -> Option<bool> {
        match self.kind() {
            SectorKind::Mode1 => {
                let stored = u32::from_le_bytes(self.bytes[2064..2068].try_into().unwrap());
                Some(edc::compute(&self.bytes[..2064]) == stored)
            }
            SectorKind::Mode2Form1 => self.edc_ok_for(false),
            SectorKind::Mode2Form2 => self.edc_ok_for(true),
            SectorKind::Cdda | SectorKind::Unknown => None,
        }
    }

    /// Build a Mode 2 sector (used by tests and by the sector extractor's
    /// fixtures). `data` is placed in the user data area and the EDC is filled
    /// in. ECC (Form 1 P/Q parity) is left zeroed; nothing in this project
    /// relies on it.
    pub fn build_mode2(lba: u32, sh: SubHeader, data: &[u8]) -> Self {
        let form2 = sh.submode.is_form2();
        let cap = if form2 {
            FORM2_DATA_SIZE
        } else {
            FORM1_DATA_SIZE
        };
        assert!(data.len() <= cap, "too much data for sector");
        let mut b = [0u8; RAW_SECTOR_SIZE];
        b[..12].copy_from_slice(&SYNC);
        let (m, s, f) = lba_to_msf(lba + 150);
        b[12] = to_bcd(m);
        b[13] = to_bcd(s);
        b[14] = to_bcd(f);
        b[15] = 2;
        b[16..20].copy_from_slice(&sh.to_bytes());
        b[20..24].copy_from_slice(&sh.to_bytes());
        b[24..24 + data.len()].copy_from_slice(data);
        let (range, at) = if form2 {
            (16..2348, 2348)
        } else {
            (16..2072, 2072)
        };
        let e = edc::compute(&b[range]);
        b[at..at + 4].copy_from_slice(&e.to_le_bytes());
        RawSector::new(lba, &b)
    }
}

fn bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0xF)
}

fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Convert an absolute sector number to minute/second/frame.
pub fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let f = lba % 75;
    let s = (lba / 75) % 60;
    let m = lba / 75 / 60;
    (m as u8, s as u8, f as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form1_roundtrip() {
        let sh = SubHeader {
            file: 1,
            channel: 2,
            submode: SubMode(SubMode::DATA),
            coding_info: 0,
        };
        let s = RawSector::build_mode2(1000, sh, b"hello");
        assert_eq!(s.kind(), SectorKind::Mode2Form1);
        assert_eq!(s.subheader(), Some(sh));
        assert_eq!(&s.user_data()[..5], b"hello");
        assert_eq!(s.user_data().len(), 2048);
        assert_eq!(s.edc_ok(), Some(true));
        assert_eq!(s.header_msf(), (0, 15, 25)); // 1150 sectors = 15s 25f
    }

    #[test]
    fn form2_edc_and_corruption() {
        let sh = SubHeader {
            file: 1,
            channel: 0,
            submode: SubMode(SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME),
            coding_info: 0x01,
        };
        let mut s = RawSector::build_mode2(5, sh, &[0x42; 2324]);
        assert_eq!(s.kind(), SectorKind::Mode2Form2);
        assert_eq!(s.user_data().len(), 2324);
        assert_eq!(s.edc_ok(), Some(true));
        s.bytes[100] ^= 1;
        assert_eq!(s.edc_ok(), Some(false));
    }

    #[test]
    fn no_sync_is_cdda() {
        let s = RawSector::new(0, &[0u8; RAW_SECTOR_SIZE]);
        assert_eq!(s.kind(), SectorKind::Cdda);
        assert_eq!(s.subheader(), None);
        assert_eq!(s.edc_ok(), None);
    }
}
