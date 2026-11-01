//! Demultiplexing: uses a disc index to pull one stream's data out of the
//! interleaved sectors.

use psx_disc::{RawSector, Result, SectorKind, SectorSource};
use psx_index::audio::XaFormat;
use psx_index::video::{StrHeader, HEADER_SIZE};
use psx_index::{AudioStream, FrameInfo, VideoStream};
use std::collections::VecDeque;

/// Bytes of frame data in each STR video sector, after the sector's header.
pub const CHUNK_DATA_SIZE: usize = 2016;
const READ_CHUNK: u32 = 1024;

/// One frame's bitstream, reassembled from its sectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxedFrame {
    /// Exactly the frame's demux size. Missing chunks are left as zeros.
    pub data: Vec<u8>,
    pub missing_chunks: u16,
}

/// Reassemble one frame of a video stream.
pub fn demux_frame(
    src: &mut dyn SectorSource,
    video: &VideoStream,
    frame: &FrameInfo,
) -> Result<DemuxedFrame> {
    let capacity = frame.chunk_count as usize * CHUNK_DATA_SIZE;
    let size = (frame.demux_size as usize).min(capacity);
    let mut buf = vec![0u8; capacity];
    let mut seen = vec![false; frame.chunk_count as usize];

    for s in src.read_range(frame.first_lba, frame.last_lba - frame.first_lba + 1)? {
        let channel = s.subheader().map_or(0, |sh| sh.channel);
        let Some(h) = StrHeader::parse(s.user_data()) else {
            continue;
        };
        let is_ours = channel == video.channel
            && h.frame_number == frame.number
            && h.chunk_count == frame.chunk_count;
        if !is_ours {
            continue;
        }
        let at = h.chunk_number as usize * CHUNK_DATA_SIZE;
        let payload = &s.user_data()[HEADER_SIZE..HEADER_SIZE + CHUNK_DATA_SIZE];
        buf[at..at + CHUNK_DATA_SIZE].copy_from_slice(payload);
        seen[h.chunk_number as usize] = true;
    }
    buf.truncate(size);
    Ok(DemuxedFrame {
        data: buf,
        missing_chunks: seen.iter().filter(|&&s| !s).count() as u16,
    })
}

fn is_stream_sector(s: &RawSector, audio: &AudioStream) -> bool {
    if s.kind() != SectorKind::Mode2Form2 {
        return false;
    }
    let Some(sh) = s.subheader() else {
        return false;
    };
    sh.submode.is_audio()
        && sh.file == audio.xa_file
        && sh.channel == audio.xa_channel
        && XaFormat::from_coding_info(sh.coding_info) == Some(audio.format)
}

/// Reads an audio stream's sectors one at a time, in disc order, without holding
/// more than a small stretch of the disc in memory.
pub struct AudioSectors<'a> {
    audio: &'a AudioStream,
    next_lba: u32,
    buffered: VecDeque<RawSector>,
}

impl<'a> AudioSectors<'a> {
    pub fn new(audio: &'a AudioStream) -> Self {
        AudioSectors {
            audio,
            next_lba: audio.first_lba,
            buffered: VecDeque::new(),
        }
    }

    /// The stream's next sector, or `None` at its end.
    pub fn next_sector(&mut self, src: &mut dyn SectorSource) -> Result<Option<RawSector>> {
        while self.buffered.is_empty() && self.next_lba <= self.audio.last_lba {
            let n = READ_CHUNK.min(self.audio.last_lba - self.next_lba + 1);
            let sectors = src.read_range(self.next_lba, n)?;
            self.buffered.extend(
                sectors
                    .into_iter()
                    .filter(|s| is_stream_sector(s, self.audio)),
            );
            self.next_lba += n;
        }
        Ok(self.buffered.pop_front())
    }
}

/// Call `f` with each sector of an audio stream, in disc order. Returns how many
/// sectors there were, which should equal the stream's `sector_count`.
pub fn for_each_audio_sector(
    src: &mut dyn SectorSource,
    audio: &AudioStream,
    mut f: impl FnMut(&[u8]),
) -> Result<u32> {
    let mut found = 0;
    let mut sectors = AudioSectors::new(audio);
    while let Some(s) = sectors.next_sector(src)? {
        f(s.user_data());
        found += 1;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_disc::source::MemoryDisc;
    use psx_disc::{SubHeader, SubMode};
    use psx_index::{scan, ScanOptions};

    fn video_sector(
        lba: u32,
        frame: u32,
        chunk: u16,
        chunks: u16,
        channel: u8,
        fill: u8,
    ) -> RawSector {
        let h = StrHeader {
            chunk_number: chunk,
            chunk_count: chunks,
            frame_number: frame,
            demux_size: 2 * CHUNK_DATA_SIZE as u32 + 100,
            width: 16,
            height: 16,
            mdec_code_count: 10,
            magic_3800: 0x3800,
            quant_scale: 1,
            version: 2,
        };
        let mut data = vec![fill; 2048];
        data[..HEADER_SIZE].copy_from_slice(&h.to_bytes());
        let sh = SubHeader {
            file: 1,
            channel,
            submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
            coding_info: 0,
        };
        RawSector::build_mode2(lba, sh, &data)
    }

    fn audio_sector(lba: u32, channel: u8, fill: u8) -> RawSector {
        let sh = SubHeader {
            file: 1,
            channel,
            submode: SubMode(SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME),
            coding_info: 0x01,
        };
        RawSector::build_mode2(lba, sh, &[fill; 2304])
    }

    fn blank(lba: u32) -> RawSector {
        RawSector::build_mode2(lba, SubHeader::default(), &[])
    }

    fn index(disc: &mut MemoryDisc) -> psx_index::DiscIndex {
        let opts = ScanOptions {
            range: None,
            source_name: "t".into(),
        };
        scan(disc, &opts, |_, _| {}).unwrap()
    }

    #[test]
    fn reassembles_chunks_around_audio_and_other_channels() {
        let mut disc = MemoryDisc {
            sectors: vec![
                blank(0),
                video_sector(1, 1, 0, 3, 1, 0xA0),
                audio_sector(2, 0, 0x55),
                video_sector(3, 1, 0, 1, 2, 0xEE), // another channel's stream
                video_sector(4, 1, 1, 3, 1, 0xA1),
                video_sector(5, 1, 2, 3, 1, 0xA2),
                video_sector(6, 2, 0, 3, 1, 0xB0),
            ],
        };
        let idx = index(&mut disc);
        let v = idx.video.iter().find(|v| v.channel == 1).unwrap();
        let f = demux_frame(&mut disc, v, &v.frames[0]).unwrap();
        assert_eq!(f.missing_chunks, 0);
        assert_eq!(f.data.len(), 2 * CHUNK_DATA_SIZE + 100);
        assert!(f.data[..CHUNK_DATA_SIZE].iter().all(|&b| b == 0xA0));
        assert!(f.data[CHUNK_DATA_SIZE..2 * CHUNK_DATA_SIZE]
            .iter()
            .all(|&b| b == 0xA1));
        assert!(f.data[2 * CHUNK_DATA_SIZE..].iter().all(|&b| b == 0xA2));
    }

    #[test]
    fn missing_chunks_stay_zero_and_get_counted() {
        let mut disc = MemoryDisc {
            sectors: vec![
                blank(0),
                video_sector(1, 1, 0, 3, 1, 0xA0),
                blank(2),
                video_sector(3, 1, 2, 3, 1, 0xA2),
            ],
        };
        let idx = index(&mut disc);
        let v = &idx.video[0];
        let f = demux_frame(&mut disc, v, &v.frames[0]).unwrap();
        assert_eq!(f.missing_chunks, 1);
        assert!(f.data[CHUNK_DATA_SIZE..2 * CHUNK_DATA_SIZE]
            .iter()
            .all(|&b| b == 0));
    }

    #[test]
    fn audio_picks_out_its_channel() {
        // Two channels, each with a sector every 8.
        let sectors = (0..20)
            .map(|lba| match lba {
                1 | 9 | 17 => audio_sector(lba, 0, 1 + lba as u8 / 8),
                2 | 10 | 18 => audio_sector(lba, 1, 9),
                _ => blank(lba),
            })
            .collect();
        let mut disc = MemoryDisc { sectors };
        let idx = index(&mut disc);
        let a = idx.audio.iter().find(|a| a.xa_channel == 0).unwrap();
        let mut firsts = Vec::new();
        let n = for_each_audio_sector(&mut disc, a, |d| firsts.push(d[0])).unwrap();
        assert_eq!(n, a.sector_count);
        assert_eq!(firsts, [1, 2, 3]);
    }
}
