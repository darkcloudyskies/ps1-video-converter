//! Throwaway prototype: decode a stream and pipe it to ffmpeg as one NUT stream,
//! with exact per-frame timestamps.
//!
//! usage: pipe-proto <disc> <index.json> <video id> <out dir> -- <ffmpeg output args...>

use anyhow::{Context, Result};
use psx_disc::BinCueDisc;
use psx_index::DiscIndex;
use psx_mdec::{decode_frame, Idct, Planes};
use psx_stream::{demux_frame, for_each_audio_sector};
use psx_xa::XaDecoder;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const FLAG_KEY: u64 = 1;
const FLAG_CODED_PTS: u64 = 8;
const FLAG_STREAM_ID: u64 = 16;
const FLAG_SIZE_MSB: u64 = 32;
const FLAG_CHECKSUM: u64 = 64;
const FLAG_CODED: u64 = 4096;
const PTS_SHIFT: u32 = 14;

fn crc(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= (b as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 { c << 1 ^ 0x04C1_1DB7 } else { c << 1 };
        }
    }
    c
}

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

struct Nut<W: Write> {
    w: W,
    pos: u64,
    last_syncpoint: u64,
}

impl<W: Write> Nut<W> {
    fn raw(&mut self, b: &[u8]) -> Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn packet(&mut self, startcode: u64, data: &[u8]) -> Result<()> {
        self.raw(&startcode.to_be_bytes())?;
        let mut hdr = Vec::new();
        put_v(&mut hdr, data.len() as u64 + 4);
        assert!(data.len() + 4 <= 4096, "would need a header checksum");
        self.raw(&hdr)?;
        self.raw(data)?;
        self.raw(&crc(data).to_be_bytes())
    }

    fn syncpoint(&mut self, pts: u64, tb_id: u64, tb_count: u64) -> Result<()> {
        let at = self.pos;
        let mut d = Vec::new();
        put_v(&mut d, pts * tb_count + tb_id);
        put_v(&mut d, (at - self.last_syncpoint + 15) / 16);
        self.last_syncpoint = at;
        self.packet(0x4E4B_E4AD_EECA_4569, &d)
    }

    fn frame(&mut self, stream: u64, pts: u64, data: &[u8]) -> Result<()> {
        let mut h = vec![0u8]; // frame code 0: everything is coded in the header
        put_v(&mut h, FLAG_KEY | FLAG_CODED_PTS | FLAG_STREAM_ID | FLAG_SIZE_MSB | FLAG_CHECKSUM);
        put_v(&mut h, stream);
        put_v(&mut h, pts + (1 << PTS_SHIFT));
        put_v(&mut h, data.len() as u64);
        let c = crc(&h);
        h.extend_from_slice(&c.to_be_bytes());
        self.raw(&h)?;
        self.raw(data)
    }
}

/// The visible frame as full-range yuv420p10le.
fn to_yuv420p10(p: &Planes, out: &mut Vec<u8>) {
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let scale = 1023.0 / 255.0;
    for row in 0..h {
        for s in &p.y[row * p.coded_width..row * p.coded_width + w] {
            let v = ((s + 128.0) * scale).round().clamp(0.0, 1023.0) as u16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    for plane in [&p.cb, &p.cr] {
        for row in 0..ch {
            let at = row * p.chroma_width();
            for s in &plane[at..at + cw] {
                let v = (s * scale + 512.0).round().clamp(0.0, 1023.0) as u16;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

enum Event {
    Frame(usize),
    Audio(usize),
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args.iter().position(|a| a == "--").context("need --")?;
    let (disc, index, id, out_dir) = (&args[0], &args[1], args[2].parse::<usize>()?, &args[3]);
    let ffmpeg_args = &args[split + 1..];

    let idx: DiscIndex = serde_json::from_slice(&std::fs::read(index)?)?;
    let v = idx.video.iter().find(|v| v.id == id).context("no stream")?;
    let a = idx.audio.iter().find(|a| v.audio.contains(&a.id)).context("no audio")?;
    let sps = v.timing.sectors_per_second as u64;
    let rate = a.format.sample_rate as u64;
    assert_eq!(rate % sps, 0);
    let base = v.first_lba.min(a.first_lba);
    let (width, height) = (v.width as u64, v.height as u64);

    let mut src = BinCueDisc::open(Path::new(disc))?;

    // Decode the audio up front, one PCM packet per sector.
    let mut dec = XaDecoder::new(a.format.channels == 2, a.format.bits_per_sample);
    let mut packets: Vec<Vec<i16>> = Vec::new();
    for_each_audio_sector(&mut src, a, |d| {
        let mut pcm = Vec::new();
        dec.decode_sector(d, &mut pcm);
        packets.push(pcm);
    })?;
    let stride = a.stride.unwrap_or(1);

    // Everything in disc order: that's already the right interleave.
    let mut events: Vec<(u32, Event)> = Vec::new();
    for (i, f) in v.frames.iter().enumerate() {
        events.push((f.last_lba, Event::Frame(i)));
    }
    for i in 0..packets.len() {
        events.push((a.first_lba + i as u32 * stride, Event::Audio(i)));
    }
    events.sort_by_key(|e| e.0);

    let out_dir = Path::new(out_dir);
    std::fs::create_dir_all(out_dir)?;
    let mut raw_v = std::io::BufWriter::new(std::fs::File::create(out_dir.join("expected.yuv"))?);
    let mut raw_a = std::io::BufWriter::new(std::fs::File::create(out_dir.join("expected.pcm"))?);
    let mut pts_v = std::io::BufWriter::new(std::fs::File::create(out_dir.join("expected_video_pts.txt"))?);
    let mut pts_a = std::io::BufWriter::new(std::fs::File::create(out_dir.join("expected_audio_pts.txt"))?);

    let mut child = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "warning", "-color_range", "pc", "-f", "nut", "-i", "pipe:0"])
        .args(ffmpeg_args)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let stdin = child.stdin.take().unwrap();
    let mut nut = Nut { w: std::io::BufWriter::with_capacity(1 << 20, stdin), pos: 0, last_syncpoint: 0 };

    nut.raw(b"nut/multimedia container\0")?;
    nut.last_syncpoint = nut.pos;

    // Main header.
    let mut d = Vec::new();
    put_v(&mut d, 3); // version
    put_v(&mut d, 2); // streams
    let max_distance = 65536;
    put_v(&mut d, max_distance);
    put_v(&mut d, 2); // time bases: 1/sps and 1/rate
    put_v(&mut d, 1);
    put_v(&mut d, sps);
    put_v(&mut d, 1);
    put_v(&mut d, rate);
    // One table entry covering all 255 usable frame codes: fully coded frames.
    put_v(&mut d, FLAG_CODED);
    put_v(&mut d, 6); // fields: pts, mul, stream, size, reserved, count
    put_v(&mut d, 0); // pts delta (signed 0)
    put_v(&mut d, 1); // size mul
    put_v(&mut d, 0); // stream
    put_v(&mut d, 0); // size lsb
    put_v(&mut d, 0); // reserved
    put_v(&mut d, 255); // count
    put_v(&mut d, 0); // elision headers, minus one: just the empty one
    nut.packet(0x4E4D_7A56_1F5F_04AD, &d)?;

    // Video stream header.
    let mut d = Vec::new();
    put_v(&mut d, 0);
    put_v(&mut d, 0); // video
    put_vb(&mut d, &[b'Y', b'3', 11, 10]);
    put_v(&mut d, 0); // time base 0
    put_v(&mut d, PTS_SHIFT as u64);
    put_v(&mut d, 1 << 30); // max pts distance
    put_v(&mut d, 0); // decode delay
    put_v(&mut d, 0); // flags
    put_vb(&mut d, &[]);
    put_v(&mut d, width);
    put_v(&mut d, height);
    put_v(&mut d, 1); // sample aspect: square, for now
    put_v(&mut d, 1);
    put_v(&mut d, 0); // colourspace: not used by ffmpeg
    nut.packet(0x4E53_1140_5BF2_F9DB, &d)?;

    // Audio stream header.
    let mut d = Vec::new();
    put_v(&mut d, 1);
    put_v(&mut d, 1); // audio
    put_vb(&mut d, b"PSD\x10");
    put_v(&mut d, 1); // time base 1
    put_v(&mut d, PTS_SHIFT as u64);
    put_v(&mut d, 1 << 30);
    put_v(&mut d, 0);
    put_v(&mut d, 0);
    put_vb(&mut d, &[]);
    put_v(&mut d, rate);
    put_v(&mut d, 1);
    put_v(&mut d, a.format.channels as u64);
    nut.packet(0x4E53_1140_5BF2_F9DB, &d)?;

    let mut buf = Vec::new();
    for (lba, ev) in &events {
        buf.clear();
        let rel = (*lba - base) as u64;
        match ev {
            Event::Frame(i) => {
                let f = &v.frames[*i];
                let dm = demux_frame(&mut src, v, f)?;
                let p = decode_frame(&dm.data, f.width, f.height, Idct::Hq)?;
                to_yuv420p10(&p, &mut buf);
                raw_v.write_all(&buf)?;
                writeln!(pts_v, "{rel}")?;
                nut.syncpoint(rel, 0, 2)?;
                nut.frame(0, rel, &buf)?;
            }
            Event::Audio(i) => {
                for s in &packets[*i] {
                    buf.extend_from_slice(&s.to_le_bytes());
                }
                raw_a.write_all(&buf)?;
                let pts = rel * (rate / sps);
                writeln!(pts_a, "{pts}")?;
                nut.syncpoint(pts, 1, 2)?;
                nut.frame(1, pts, &buf)?;
            }
        }
    }
    nut.w.flush()?;
    drop(nut);
    let status = child.wait()?;
    println!("{} frames, {} audio packets, ffmpeg: {status}", v.frames.len(), packets.len());
    Ok(())
}
