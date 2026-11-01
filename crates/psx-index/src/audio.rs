//! CD-ROM XA ADPCM audio format, from the subheader coding-info byte.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct XaFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub emphasis: bool,
}

impl XaFormat {
    /// Decode the coding-info byte; `None` if it uses reserved values.
    pub fn from_coding_info(ci: u8) -> Option<Self> {
        let channels = match ci & 0x03 {
            0 => 1,
            1 => 2,
            _ => return None,
        };
        let sample_rate = match (ci >> 2) & 0x03 {
            0 => 37_800,
            1 => 18_900,
            _ => return None,
        };
        let bits_per_sample = match (ci >> 4) & 0x03 {
            0 => 4,
            1 => 8,
            _ => return None,
        };
        Some(XaFormat {
            sample_rate,
            channels,
            bits_per_sample,
            emphasis: ci & 0x40 != 0,
        })
    }

    /// PCM samples per channel carried by one audio sector
    /// (18 sound groups × 224 or 112 samples).
    pub fn samples_per_channel_per_sector(&self) -> u32 {
        let total = if self.bits_per_sample == 4 {
            18 * 224
        } else {
            18 * 112
        };
        total / self.channels as u32
    }

    /// Disc read speed (sectors per second) implied by this format played at
    /// one audio sector every `stride` sectors.
    pub fn implied_sectors_per_second(&self, stride: u32) -> f64 {
        stride as f64 * self.sample_rate as f64 / self.samples_per_channel_per_sector() as f64
    }

    /// The standard disc speed (75 or 150 sectors per second) this format
    /// plays at with this `stride`, if any.
    pub fn disc_speed(&self, stride: u32) -> Option<u32> {
        let sps = self.implied_sectors_per_second(stride);
        [75u32, 150]
            .into_iter()
            .find(|&std| (sps - std as f64).abs() / (std as f64) < 0.02)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_37800_every_8_sectors_is_double_speed() {
        let f = XaFormat::from_coding_info(0x01).unwrap();
        assert_eq!(
            (f.channels, f.sample_rate, f.bits_per_sample),
            (2, 37_800, 4)
        );
        assert_eq!(f.samples_per_channel_per_sector(), 2016);
        assert!((f.implied_sectors_per_second(8) - 150.0).abs() < 1e-9);
        assert_eq!(f.disc_speed(8), Some(150));
        assert_eq!(f.disc_speed(4), Some(75));
        assert_eq!(f.disc_speed(66), None);
    }

    #[test]
    fn reserved_values_rejected() {
        assert!(XaFormat::from_coding_info(0x02).is_none());
        assert!(XaFormat::from_coding_info(0x08).is_none());
        assert!(XaFormat::from_coding_info(0x20).is_none());
    }
}
