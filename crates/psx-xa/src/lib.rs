//! CD-ROM XA ADPCM decoding.
//!
//! An audio sector holds 18 sound groups of 128 bytes: 16 bytes of parameters,
//! then 112 bytes of samples interleaved across the group's sound units. Each
//! unit decodes to 28 PCM samples, and there are 8 units at 4 bits per sample or
//! 4 at 8 bits. In stereo, even units are left and odd units are right.

/// Bytes of ADPCM data used in an audio sector; the rest of the Form 2 user
/// data area is unused.
pub const SECTOR_DATA_SIZE: usize = 2304;
const GROUP_SIZE: usize = 128;
const PARAMS_SIZE: usize = 16;
const SAMPLES_PER_UNIT: usize = 28;

/// Prediction filter coefficients, selected by each unit's parameter byte.
const K0: [f64; 4] = [0.0, 60.0 / 64.0, 115.0 / 64.0, 98.0 / 64.0];
const K1: [f64; 4] = [0.0, 0.0, -52.0 / 64.0, -55.0 / 64.0];

/// Prediction history for one channel. It's kept at full precision, unrounded
/// and unclamped, so rounding errors don't feed back into later samples.
#[derive(Debug, Clone, Copy, Default)]
struct History {
    prev1: f64,
    prev2: f64,
}

/// Which channel's history a sound unit continues. Mono uses `Left`.
#[derive(Debug, Clone, Copy)]
enum Side {
    Left,
    Right,
}

/// Choose between a unit's redundant copies of its parameter byte. On a clean
/// disc they all agree. Otherwise prefer valid values (the top two bits clear),
/// then the most frequent, then the earliest.
fn best_param(copies: &[u8]) -> u8 {
    let valid = |p: u8| p & 0xC0 == 0;
    let count = |p: u8| copies.iter().filter(|&&c| c == p).count();
    let mut best = copies[0];
    for &p in copies {
        if (valid(p) && !valid(best)) || (valid(p) == valid(best) && count(p) > count(best)) {
            best = p;
        }
    }
    best
}

/// Decodes the audio sectors of one stream, keeping state from sector to sector.
#[derive(Debug, Clone)]
pub struct XaDecoder {
    stereo: bool,
    bits_per_sample: u8,
    left: History,
    right: History,
}

impl XaDecoder {
    /// # Panics
    /// If `bits_per_sample` isn't 4 or 8.
    pub fn new(stereo: bool, bits_per_sample: u8) -> Self {
        assert!(matches!(bits_per_sample, 4 | 8), "XA is 4 or 8 bits");
        XaDecoder {
            stereo,
            bits_per_sample,
            left: History::default(),
            right: History::default(),
        }
    }

    fn units_per_group(&self) -> usize {
        if self.bits_per_sample == 4 {
            8
        } else {
            4
        }
    }

    /// One unit's parameter byte, from the group's repeated copies.
    fn unit_param(&self, group: &[u8], unit: usize) -> u8 {
        if self.bits_per_sample == 4 {
            // Laid out 0123 0123 4567 4567.
            let first = unit % 4 + (unit / 4) * 8;
            best_param(&[group[first], group[first + 4]])
        } else {
            // Laid out 0123 0123 0123 0123.
            best_param(&[
                group[unit],
                group[unit + 4],
                group[unit + 8],
                group[unit + 12],
            ])
        }
    }

    /// One raw sample of a unit, placed in the top of a 16-bit range.
    fn unit_sample(&self, samples: &[u8], unit: usize, i: usize) -> i32 {
        if self.bits_per_sample == 4 {
            let byte = samples[i * 4 + unit / 2] as i32;
            let nibble = if unit % 2 == 0 {
                byte & 0x0F
            } else {
                byte >> 4
            };
            (((nibble << 12) as u16) as i16) as i32
        } else {
            ((samples[i * 4 + unit] as u16) << 8) as i16 as i32
        }
    }

    fn decode_unit(&mut self, group: &[u8], unit: usize, side: Side) -> [i16; SAMPLES_PER_UNIT] {
        let mut h = match side {
            Side::Left => self.left,
            Side::Right => self.right,
        };
        let param = self.unit_param(group, unit);
        let range = (param & 0x0F) as u32;
        let filter = ((param >> 4) as usize).min(3);
        let samples = &group[PARAMS_SIZE..];

        let mut out = [0i16; SAMPLES_PER_UNIT];
        for (i, o) in out.iter_mut().enumerate() {
            let raw = self.unit_sample(samples, unit, i) >> range;
            let pcm = raw as f64 + K0[filter] * h.prev1 + K1[filter] * h.prev2;
            h.prev2 = h.prev1;
            h.prev1 = pcm;
            *o = pcm.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        match side {
            Side::Left => self.left = h,
            Side::Right => self.right = h,
        }
        out
    }

    /// Decode a sector, appending PCM to `out` (interleaved if stereo).
    ///
    /// # Panics
    /// If `sector` is shorter than [`SECTOR_DATA_SIZE`].
    pub fn decode_sector(&mut self, sector: &[u8], out: &mut Vec<i16>) {
        let units = self.units_per_group();
        for group in sector[..SECTOR_DATA_SIZE].chunks_exact(GROUP_SIZE) {
            if self.stereo {
                for unit in (0..units).step_by(2) {
                    let l = self.decode_unit(group, unit, Side::Left);
                    let r = self.decode_unit(group, unit + 1, Side::Right);
                    for (a, b) in l.iter().zip(&r) {
                        out.push(*a);
                        out.push(*b);
                    }
                }
            } else {
                for unit in 0..units {
                    out.extend_from_slice(&self.decode_unit(group, unit, Side::Left));
                }
            }
        }
    }
}

/// Write 16-bit PCM as a WAV file.
pub fn write_wav<W: std::io::Write>(
    out: &mut W,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> std::io::Result<()> {
    let data_size = (samples.len() * 2) as u32;
    let block_align = channels * 2;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_size).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&sample_rate.to_le_bytes())?;
    out.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
    out.write_all(&block_align.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?; // bits per sample
    out.write_all(b"data")?;
    out.write_all(&data_size.to_le_bytes())?;
    for s in samples {
        out.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sector with `param` on every unit and `byte` as every sample byte.
    fn sector(param: u8, byte: u8) -> Vec<u8> {
        let mut s = vec![byte; SECTOR_DATA_SIZE];
        for group in s.chunks_exact_mut(GROUP_SIZE) {
            group[..PARAMS_SIZE].fill(param);
        }
        s
    }

    fn decode(stereo: bool, bits: u8, sector: &[u8]) -> Vec<i16> {
        let mut d = XaDecoder::new(stereo, bits);
        let mut out = Vec::new();
        d.decode_sector(sector, &mut out);
        out
    }

    #[test]
    fn sample_counts_match_the_format() {
        let s = sector(0, 0);
        assert_eq!(decode(true, 4, &s).len(), 2016 * 2);
        assert_eq!(decode(false, 4, &s).len(), 4032);
        assert_eq!(decode(true, 8, &s).len(), 1008 * 2);
        assert_eq!(decode(false, 8, &s).len(), 2016);
    }

    #[test]
    fn range_shifts_the_raw_sample() {
        // Filter 0 means no prediction: nibble 1 << 12 >> range.
        let out = decode(false, 4, &sector(0x04, 0x11));
        assert!(out.iter().all(|&v| v == 0x1000 >> 4));
        // The top bit of a nibble is its sign: 0xF = -1.
        let out = decode(false, 4, &sector(0x00, 0xFF));
        assert!(out.iter().all(|&v| v == -0x1000));
        // 8-bit: the byte goes in the top of the range.
        let out = decode(false, 8, &sector(0x08, 0x40));
        assert!(out.iter().all(|&v| v == 0x40));
        let out = decode(false, 8, &sector(0x08, 0xC0));
        assert!(out.iter().all(|&v| v == -0x40));
    }

    #[test]
    fn nibbles_go_low_then_high_across_units() {
        let mut s = sector(0x00, 0);
        // First byte of the samples: unit 0 in the low nibble, unit 1 in the high.
        s[PARAMS_SIZE] = 0x21;
        let out = decode(true, 4, &s);
        assert_eq!(out[0], 0x1000, "left");
        assert_eq!(out[1], 0x2000, "right");
    }

    #[test]
    fn stereo_channels_predict_separately() {
        // Filter 1 everywhere. Left's raw samples are all 1 and right's are all
        // 0, so right only leaves zero if it picks up left's history.
        let out = decode(true, 4, &sector(0x1C, 0x01));
        assert_eq!(out[0], 1);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 2, "1 + 0.9375 × 1, rounded");
        assert_eq!(out[3], 0);
    }

    #[test]
    fn filters_use_the_standard_coefficients_and_full_precision_history() {
        // Filter 2 with a constant input of 64: 64, then 64 + 115/64 × 64 = 179,
        // then 64 + 115/64 × 179 − 52/64 × 64 = 333.640625, which is kept in full.
        let mut d = XaDecoder::new(false, 8);
        let mut s = sector(0x28, 0x40); // range 8: 0x4000 >> 8 = 64
        for group in s.chunks_exact_mut(GROUP_SIZE) {
            group[..PARAMS_SIZE].fill(0x28);
        }
        let mut out = Vec::new();
        d.decode_sector(&s, &mut out);
        assert_eq!(out[0], 64);
        assert_eq!(out[1], 179);
        assert_eq!(out[2], 334);
        let fourth: f64 = 64.0 + 115.0 / 64.0 * 333.640625 - 52.0 / 64.0 * 179.0;
        assert_eq!(out[3], fourth.round() as i16);
    }

    #[test]
    fn output_clamps_but_history_does_not() {
        // A filter that grows quickly until it saturates.
        let out = decode(false, 4, &sector(0x20, 0x77));
        assert!(out.contains(&i16::MAX));
    }

    #[test]
    fn state_carries_across_sectors() {
        let s = sector(0x1C, 0x11);
        let mut d = XaDecoder::new(false, 4);
        let mut out = Vec::new();
        d.decode_sector(&s, &mut out);
        let boundary = out.len();
        d.decode_sector(&s, &mut out);
        // Filter 1 approaches 1 / (1 − 0.9375) = 16; a reset would restart at 1.
        assert!(out[boundary] > 10, "{}", out[boundary]);
    }

    #[test]
    fn redundant_parameters_vote() {
        assert_eq!(best_param(&[5, 5, 5, 5]), 5);
        assert_eq!(best_param(&[1, 2, 2, 3]), 2);
        assert_eq!(best_param(&[0xC5, 0x05]), 0x05, "prefers a valid value");
        assert_eq!(best_param(&[1, 2]), 1, "ties keep the earliest");
    }

    #[test]
    fn wav_header_is_well_formed() {
        let mut b = Vec::new();
        write_wav(&mut b, 37_800, 2, &[1, -1, 2, -2]).unwrap();
        assert_eq!(b.len(), 44 + 8);
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(b[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 37_800);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 151_200);
        assert_eq!(u16::from_le_bytes(b[32..34].try_into().unwrap()), 4);
        assert_eq!(&b[36..40], b"data");
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 8);
        assert_eq!(&b[44..46], &1i16.to_le_bytes());
    }
}
