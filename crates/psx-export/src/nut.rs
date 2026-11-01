//! A minimal NUT muxer: just enough to hand FFmpeg raw video and PCM with exact
//! rational timestamps. Every frame is a key frame with a syncpoint before it.

use std::io::{Result, Write};

const FILE_ID: &[u8] = b"nut/multimedia container\0";
const MAIN_STARTCODE: u64 = 0x4E4D_7A56_1F5F_04AD;
const STREAM_STARTCODE: u64 = 0x4E53_1140_5BF2_F9DB;
const SYNCPOINT_STARTCODE: u64 = 0x4E4B_E4AD_EECA_4569;

const FLAG_KEY: u64 = 1;
const FLAG_CODED_PTS: u64 = 8;
const FLAG_STREAM_ID: u64 = 16;
const FLAG_SIZE_MSB: u64 = 32;
const FLAG_CHECKSUM: u64 = 64;
const FLAG_CODED: u64 = 4096;

const PTS_SHIFT: u64 = 14;
const MAX_DISTANCE: u64 = 65536;
/// Packets above this size would need a header checksum. Ours never get close.
const MAX_PACKET: usize = 4096;

/// A stream in the NUT file. Its time base is 1/`time_base_den`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Video {
        fourcc: [u8; 4],
        width: u16,
        height: u16,
        sar: (u32, u32),
        time_base_den: u32,
    },
    /// 16-bit little-endian PCM, timed in samples.
    Audio { sample_rate: u32, channels: u8 },
}

impl Stream {
    fn time_base_den(&self) -> u32 {
        match *self {
            Stream::Video { time_base_den, .. } => time_base_den,
            Stream::Audio { sample_rate, .. } => sample_rate,
        }
    }
}

/// CRC-32 with polynomial 0x04C11DB7 and initial value 0, as NUT specifies.
fn crc(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= (b as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                c << 1 ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
    }
    c
}

/// NUT's variable-length integer: 7 bits per byte, most significant first.
fn put_v(out: &mut Vec<u8>, v: u64) {
    let mut n = 1;
    while n < 10 && v >> (7 * n) != 0 {
        n += 1;
    }
    for i in (1..n).rev() {
        out.push(((v >> (7 * i)) & 0x7F) as u8 | 0x80);
    }
    out.push((v & 0x7F) as u8);
}

fn put_vb(out: &mut Vec<u8>, data: &[u8]) {
    put_v(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Writes a NUT file. Frames must arrive in timestamp order across streams.
pub struct NutWriter<W: Write> {
    w: W,
    pos: u64,
    last_syncpoint: u64,
    /// Each distinct time base's denominator. NUT numbers time bases from 0.
    time_bases: Vec<u32>,
    /// Each stream's index into `time_bases`.
    stream_time_base: Vec<usize>,
}

impl<W: Write> NutWriter<W> {
    /// Write the file's headers.
    pub fn new(w: W, streams: &[Stream]) -> Result<Self> {
        let mut time_bases = Vec::new();
        let mut stream_time_base = Vec::new();
        for s in streams {
            let den = s.time_base_den();
            let at = time_bases
                .iter()
                .position(|&d| d == den)
                .unwrap_or_else(|| {
                    time_bases.push(den);
                    time_bases.len() - 1
                });
            stream_time_base.push(at);
        }
        let mut nut = NutWriter {
            w,
            pos: 0,
            last_syncpoint: 0,
            time_bases,
            stream_time_base,
        };
        nut.raw(FILE_ID)?;
        nut.last_syncpoint = nut.pos;
        nut.main_header(streams.len() as u64)?;
        for (id, s) in streams.iter().enumerate() {
            nut.stream_header(id, s)?;
        }
        Ok(nut)
    }

    fn raw(&mut self, b: &[u8]) -> Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn packet(&mut self, startcode: u64, data: &[u8]) -> Result<()> {
        assert!(data.len() + 4 <= MAX_PACKET, "would need a header checksum");
        self.raw(&startcode.to_be_bytes())?;
        let mut size = Vec::new();
        put_v(&mut size, data.len() as u64 + 4);
        self.raw(&size)?;
        self.raw(data)?;
        self.raw(&crc(data).to_be_bytes())
    }

    fn main_header(&mut self, stream_count: u64) -> Result<()> {
        let mut d = Vec::new();
        put_v(&mut d, 3); // version
        put_v(&mut d, stream_count);
        put_v(&mut d, MAX_DISTANCE);
        put_v(&mut d, self.time_bases.len() as u64);
        for &den in &self.time_bases {
            put_v(&mut d, 1);
            put_v(&mut d, den as u64);
        }
        // One table entry covering all 255 usable frame codes: fully coded frames.
        put_v(&mut d, FLAG_CODED);
        put_v(&mut d, 6); // fields: pts, mul, stream, size, reserved, count
        put_v(&mut d, 0); // pts delta (signed 0)
        put_v(&mut d, 1); // size mul
        put_v(&mut d, 0); // stream
        put_v(&mut d, 0); // size lsb
        put_v(&mut d, 0); // reserved
        put_v(&mut d, 255); // count
                            // Optional in the spec, but FFmpeg rejects frames with `header_idx
                            // invalid` without it. Elision headers minus one: just the empty one.
        put_v(&mut d, 0);
        self.packet(MAIN_STARTCODE, &d)
    }

    fn stream_header(&mut self, id: usize, stream: &Stream) -> Result<()> {
        let mut d = Vec::new();
        put_v(&mut d, id as u64);
        let class = match stream {
            Stream::Video { .. } => 0,
            Stream::Audio { .. } => 1,
        };
        put_v(&mut d, class);
        match stream {
            Stream::Video { fourcc, .. } => put_vb(&mut d, fourcc),
            Stream::Audio { .. } => put_vb(&mut d, b"PSD\x10"),
        }
        put_v(&mut d, self.stream_time_base[id] as u64);
        put_v(&mut d, PTS_SHIFT);
        put_v(&mut d, 1 << 30); // max pts distance
        put_v(&mut d, 0); // decode delay
        put_v(&mut d, 0); // flags
        put_vb(&mut d, &[]); // codec-specific data
        match *stream {
            Stream::Video {
                width, height, sar, ..
            } => {
                put_v(&mut d, width as u64);
                put_v(&mut d, height as u64);
                put_v(&mut d, sar.0 as u64);
                put_v(&mut d, sar.1 as u64);
                put_v(&mut d, 0); // colourspace: FFmpeg doesn't use it
            }
            Stream::Audio {
                sample_rate,
                channels,
                ..
            } => {
                put_v(&mut d, sample_rate as u64);
                put_v(&mut d, 1); // sample rate denominator
                put_v(&mut d, channels as u64);
            }
        }
        self.packet(STREAM_STARTCODE, &d)
    }

    fn syncpoint(&mut self, stream: usize, pts: u64) -> Result<()> {
        let at = self.pos;
        let mut d = Vec::new();
        put_v(
            &mut d,
            pts * self.time_bases.len() as u64 + self.stream_time_base[stream] as u64,
        );
        put_v(&mut d, (at - self.last_syncpoint).div_ceil(16));
        self.last_syncpoint = at;
        self.packet(SYNCPOINT_STARTCODE, &d)
    }

    /// Write one frame, with `pts` in the stream's time base.
    pub fn frame(&mut self, stream: usize, pts: u64, data: &[u8]) -> Result<()> {
        self.syncpoint(stream, pts)?;
        let mut h = vec![0u8]; // frame code 0: everything is coded in the header
                               // The checksum is required on frames larger than 2 * max_distance.
        put_v(
            &mut h,
            FLAG_KEY | FLAG_CODED_PTS | FLAG_STREAM_ID | FLAG_SIZE_MSB | FLAG_CHECKSUM,
        );
        put_v(&mut h, stream as u64);
        put_v(&mut h, pts + (1 << PTS_SHIFT));
        put_v(&mut h, data.len() as u64);
        let c = crc(&h);
        h.extend_from_slice(&c.to_be_bytes());
        self.raw(&h)?;
        self.raw(data)
    }

    /// Flush and hand back the underlying writer.
    pub fn finish(mut self) -> Result<W> {
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_v(&mut out, value);
        out
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        // CRC-32/CKSUM's published check value is 0x765E7680, which includes a
        // final inversion that NUT's doesn't have.
        assert_eq!(crc(b"123456789"), !0x765E_7680);
        assert_eq!(crc(b""), 0);
    }

    #[test]
    fn varints_put_the_most_significant_bits_first() {
        assert_eq!(v(0), [0]);
        assert_eq!(v(127), [127]);
        assert_eq!(v(128), [0x81, 0]);
        assert_eq!(v(16384), [0x81, 0x80, 0]);
    }

    fn video(den: u32) -> Stream {
        Stream::Video {
            fourcc: *b"I420",
            width: 16,
            height: 16,
            sar: (1, 1),
            time_base_den: den,
        }
    }

    #[test]
    fn streams_with_the_same_rate_share_a_time_base() {
        let audio = |sample_rate| Stream::Audio {
            sample_rate,
            channels: 2,
        };
        let streams = [video(150), audio(37_800), audio(37_800), audio(18_900)];
        let nut = NutWriter::new(Vec::new(), &streams).unwrap();
        assert_eq!(nut.time_bases, [150, 37_800, 18_900]);
        assert_eq!(nut.stream_time_base, [0, 1, 1, 2]);
    }

    #[test]
    fn syncpoint_key_encodes_the_time_base() {
        let streams = [
            video(150),
            Stream::Audio {
                sample_rate: 37_800,
                channels: 2,
            },
        ];
        let mut nut = NutWriter::new(Vec::new(), &streams).unwrap();
        let before = nut.pos as usize;
        nut.frame(1, 10, b"abcd").unwrap();
        let out = nut.finish().unwrap();
        let sync = &out[before..];
        assert_eq!(&sync[..8], &SYNCPOINT_STARTCODE.to_be_bytes());
        // Packet size, then 10 × 2 time bases + time base 1 = 21.
        assert_eq!(sync[9], 21);
        assert_eq!(&out[out.len() - 4..], b"abcd");
    }
}
