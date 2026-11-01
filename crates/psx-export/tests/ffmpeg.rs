//! End-to-end checks through a real FFmpeg: exact timestamps, tags and losslessness.
//!
//! These skip themselves if `ffmpeg` and `ffprobe` aren't on PATH. To also check
//! real discs, point `PSXVC_TEST_DISCS` at a folder of `.cue` files (an absolute
//! path, since tests run from the crate's folder) and use `--release`, since it
//! decodes each disc's shortest constant and variable frame rate streams:
//!
//! ```text
//! $env:PSXVC_TEST_DISCS = "$PWD\discs"; cargo test --release -p psx-export -- --nocapture
//! ```

use psx_disc::source::MemoryDisc;
use psx_disc::{BinCueDisc, RawSector, SectorSource, SubHeader, SubMode};
use psx_export::{
    base_sector, constant_rate, export_stream, frame_bytes, AudioCodec, Error, ExportOptions,
    PixelFormat, Preset,
};
use psx_index::video::StrHeader;
use psx_index::{scan, AudioStream, DiscIndex, ScanOptions, VideoStream};
use psx_mdec::testutil::encode_frame;
use psx_mdec::{decode_frame, Block, Idct};
use psx_stream::{demux_frame, for_each_audio_sector};
use psx_xa::XaDecoder;
use std::path::{Path, PathBuf};
use std::process::Command;

fn have_ffmpeg() -> bool {
    let works = |p: &str| {
        Command::new(p)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let ok = works("ffmpeg") && works("ffprobe");
    if !ok {
        eprintln!("skipping: ffmpeg and ffprobe aren't on PATH");
    }
    ok
}

fn output_of(program: &str, args: &[&str]) -> Vec<u8> {
    let o = Command::new(program).args(args).output().unwrap();
    assert!(
        o.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout
}

fn options(preset: Preset, idct: Idct, sar: (u32, u32)) -> ExportOptions {
    ExportOptions {
        preset,
        idct,
        sar,
        audio: None,
        cfr: false,
        ffmpeg: PathBuf::from("ffmpeg"),
    }
}

const FIRST_LBA: u32 = 30;
/// Sectors between the end of one frame and the end of the next.
const GAPS: [u32; 5] = [9, 11, 10, 9, 12];
const WIDTH: u16 = 32;
const HEIGHT: u16 = 32;

/// A 2x-speed stream with audio every 8 sectors and true variable frame rate. Each
/// frame fits in one sector and has its own brightness, with some ADPCM that changes
/// from sector to sector.
fn synthetic_disc() -> MemoryDisc {
    let mut disc = MemoryDisc {
        sectors: (0..120)
            .map(|lba| RawSector::build_mode2(lba, SubHeader::default(), &[]))
            .collect(),
    };
    let audio = SubHeader {
        file: 1,
        channel: 0,
        submode: SubMode(SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME),
        coding_info: 0x01,
    };
    let is_audio = |lba: u32| (lba - FIRST_LBA) % 8 == 0;
    for lba in (FIRST_LBA..110).filter(|&l| is_audio(l)) {
        // Noise, so that a shift in time shows up when comparing lossy audio.
        let mut noise = lba.wrapping_mul(2_654_435_761) | 1;
        let mut pcm: Vec<u8> = (0..2304)
            .map(|_| {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                noise as u8
            })
            .collect();
        for group in pcm.chunks_exact_mut(128) {
            group[..16].fill(0x04);
        }
        disc.sectors[lba as usize] = RawSector::build_mode2(lba, audio, &pcm);
    }

    let video = SubHeader {
        file: 1,
        channel: 1,
        submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
        coding_info: 0,
    };
    let mut lba = FIRST_LBA + 1;
    for number in 0..=GAPS.len() {
        while is_audio(lba) {
            lba += 1;
        }
        let blocks: Vec<Block> = (0..4 * 6)
            .map(|i| {
                let mut coefs = [0i16; 64];
                if i % 6 >= 2 {
                    coefs[0] = -300 + 100 * number as i16 + 10 * i as i16;
                }
                Block { qscale: 1, coefs }
            })
            .collect();
        let frame = encode_frame(&blocks, 1, 2);
        let h = StrHeader {
            chunk_number: 0,
            chunk_count: 1,
            frame_number: number as u32 + 1,
            demux_size: frame.len() as u32,
            width: WIDTH,
            height: HEIGHT,
            mdec_code_count: u16::from_le_bytes([frame[0], frame[1]]),
            magic_3800: 0x3800,
            quant_scale: 1,
            version: 2,
        };
        let mut data = vec![0u8; 2048];
        data[..32].copy_from_slice(&h.to_bytes());
        data[32..32 + frame.len()].copy_from_slice(&frame);
        disc.sectors[lba as usize] = RawSector::build_mode2(lba, video, &data);
        lba += GAPS.get(number).copied().unwrap_or(0);
    }
    disc
}

fn index_of(disc: &mut dyn SectorSource) -> DiscIndex {
    let opts = ScanOptions {
        range: None,
        source_name: "t".into(),
    };
    scan(disc, &opts, |_, _| {}).unwrap()
}

fn audio_of<'a>(idx: &'a DiscIndex, v: &VideoStream) -> Vec<&'a AudioStream> {
    idx.audio
        .iter()
        .filter(|a| v.audio.contains(&a.id))
        .collect()
}

/// What the stream should decode to, worked out without going through the exporter.
fn expected_video(src: &mut dyn SectorSource, v: &VideoStream, idct: Idct) -> Vec<u8> {
    let format = PixelFormat::for_export(Preset::Archive, idct);
    let mut out = Vec::new();
    for f in &v.frames {
        let d = demux_frame(src, v, f).unwrap();
        let p = decode_frame(&d.data, f.width, f.height, idct).unwrap();
        frame_bytes(&p, format, &mut out);
    }
    out
}

/// What constant frame rate video should decode to: for each tick, the last frame to
/// land on it or before it. Ticks before the first frame show the first frame.
fn expected_cfr_video(
    src: &mut dyn SectorSource,
    v: &VideoStream,
    base: u32,
    rate: (u32, u32),
    idct: Idct,
) -> Vec<u8> {
    let format = PixelFormat::for_export(Preset::Archive, idct);
    let scale = (v.timing.sectors_per_second * rate.1) as u64;
    let mut ticks: Vec<(u64, Vec<u8>)> = Vec::new();
    for f in &v.frames {
        let d = demux_frame(src, v, f).unwrap();
        let p = decode_frame(&d.data, f.width, f.height, idct).unwrap();
        let mut bytes = Vec::new();
        frame_bytes(&p, format, &mut bytes);
        let tick = ((f.last_lba - base) as u64 * rate.0 as u64 + scale / 2) / scale;
        match ticks.last_mut() {
            Some(last) if last.0 == tick => last.1 = bytes,
            _ => ticks.push((tick, bytes)),
        }
    }
    let mut out = Vec::new();
    let mut at = 0;
    for (i, (tick, bytes)) in ticks.iter().enumerate() {
        // Each frame holds until the next one's tick.
        let until = ticks.get(i + 1).map_or(tick + 1, |n| n.0);
        for _ in at..until {
            out.extend_from_slice(bytes);
        }
        at = until;
    }
    out
}

/// The lag in seconds that best lines lossy audio up with the original. Both are
/// compared as left-channel samples at the original's rate, from time zero.
fn audio_lag(
    src: &mut dyn SectorSource,
    file: &Path,
    v: &VideoStream,
    a: &AudioStream,
    base: u32,
) -> Option<f64> {
    let rate = a.format.sample_rate;
    let channels = a.format.channels as usize;
    let filter = format!("aresample={rate}:async=1:first_pts=0");
    let got = decoded_back(file, "0:a:0", &["-af", &filter, "-f", "s16le"]);
    let left = |b: &[u8]| -> Vec<f64> {
        b.chunks_exact(2 * channels)
            .map(|s| i16::from_le_bytes([s[0], s[1]]) as f64)
            .collect()
    };
    let got = left(&got);
    let start =
        (a.first_lba - base) as usize * rate as usize / v.timing.sectors_per_second as usize;
    let want = left(&expected_audio(src, a));

    // Compare the loudest stretch, since streams often open with silence, which
    // matches at any lag. A silent stream has nothing to measure.
    const LEN: usize = 8000;
    let energy = |w: &[f64]| w.iter().map(|s| s * s).sum::<f64>();
    let (at, want) = want
        .chunks(LEN)
        .enumerate()
        .max_by(|x, y| energy(x.1).total_cmp(&energy(y.1)))
        .map(|(i, w)| (i * LEN, w))?;
    if energy(want) == 0.0 {
        return None;
    }
    let max_lag = rate as usize / 25;
    let mut best = (f64::MIN, 0i64);
    for lag in -(max_lag as i64)..=max_lag as i64 {
        let from = (start + at) as i64 + lag;
        let Some(window) = usize::try_from(from)
            .ok()
            .and_then(|from| got.get(from..from + want.len()))
        else {
            continue;
        };
        let score: f64 = window.iter().zip(want).map(|(x, y)| x * y).sum();
        if score > best.0 {
            best = (score, lag);
        }
    }
    Some(best.1 as f64 / rate as f64)
}

fn check_audio_lag(src: &mut dyn SectorSource, file: &Path, v: &VideoStream, a: &AudioStream) {
    let base = base_sector(v, &[a]);
    let Some(lag) = audio_lag(src, file, v, a, base) else {
        eprintln!("{file:?}: audio lag not measured: the stream is silent");
        return;
    };
    eprintln!("{file:?}: audio lag {:.3} ms", lag * 1000.0);
    // Make sure the measurement can see a shift: moving the base a sector should
    // show up as that sector's length.
    let sector = 1.0 / v.timing.sectors_per_second as f64;
    let shifted = audio_lag(src, file, v, a, base - 1).expect("it wasn't silent before");
    assert!(
        (shifted - lag + sector).abs() <= LOSSY_AUDIO_TOLERANCE,
        "{file:?}: lag measured {shifted} s with the base a sector earlier, against {lag} s"
    );
    assert!(
        lag.abs() <= LOSSY_AUDIO_TOLERANCE,
        "{file:?}: audio is {lag} s out"
    );
}

fn expected_audio(src: &mut dyn SectorSource, a: &AudioStream) -> Vec<u8> {
    let mut dec = XaDecoder::new(a.format.channels == 2, a.format.bits_per_sample);
    let mut pcm = Vec::new();
    for_each_audio_sector(src, a, |d| dec.decode_sector(d, &mut pcm)).unwrap();
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn decoded_back(file: &Path, map: &str, args: &[&str]) -> Vec<u8> {
    let file = file.to_str().unwrap();
    let mut a = vec!["-v", "error", "-i", file, "-map", map];
    a.extend_from_slice(args);
    a.extend_from_slice(&["pipe:1"]);
    output_of("ffmpeg", &a)
}

fn packet_times(file: &Path, stream: &str) -> Vec<f64> {
    let out = output_of(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ],
    );
    let mut t: Vec<f64> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches(',').parse().unwrap())
        .collect();
    t.sort_by(f64::total_cmp);
    t
}

fn stream_field(file: &Path, stream: &str, field: &str) -> String {
    let out = output_of(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            &format!("stream={field}"),
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ],
    );
    String::from_utf8(out)
        .unwrap()
        .trim()
        .trim_end_matches(',')
        .to_string()
}

/// Every video timestamp must land on its sector, within `tolerance` seconds.
fn check_video_times(file: &Path, v: &VideoStream, base: u32, tolerance: f64) {
    let got = packet_times(file, "v");
    assert_eq!(got.len(), v.frames.len(), "frame count of {file:?}");
    let sps = v.timing.sectors_per_second as f64;
    for (t, f) in got.iter().zip(&v.frames) {
        let want = (f.last_lba - base) as f64 / sps;
        assert!(
            (t - want).abs() <= tolerance,
            "frame {} of {file:?}: at {t}, wanted {want}",
            f.number
        );
    }
}

/// The first audio packet must start at its sector, within `tolerance` seconds.
fn check_audio_start(file: &Path, v: &VideoStream, a: &AudioStream, base: u32, tolerance: f64) {
    let got = packet_times(file, "a");
    let want = (a.first_lba - base) as f64 / v.timing.sectors_per_second as f64;
    assert!(
        (got[0] - want).abs() <= tolerance,
        "audio of {file:?}: starts at {}, wanted {want}",
        got[0]
    );
}

fn check_tags(file: &Path, pix_fmt: &str, chroma: &str) {
    assert_eq!(stream_field(file, "v", "pix_fmt"), pix_fmt, "{file:?}");
    assert_eq!(stream_field(file, "v", "color_range"), "pc", "{file:?}");
    for field in ["color_space", "color_primaries", "color_transfer"] {
        assert_eq!(stream_field(file, "v", field), "smpte170m", "{file:?}");
    }
    assert_eq!(
        stream_field(file, "v", "chroma_location"),
        chroma,
        "{file:?}"
    );
}

/// MKV rounds to 1 ms, MP4 keeps sector-exact timestamps up to ffprobe's printing.
const MKV_TOLERANCE: f64 = 0.001;
const MP4_TOLERANCE: f64 = 0.000_001;
/// Lossy audio has to line up to within a millisecond of where it started out.
const LOSSY_AUDIO_TOLERANCE: f64 = 0.001;

fn check_archive(src: &mut dyn SectorSource, v: &VideoStream, audio: &[&AudioStream], dir: &Path) {
    let base = base_sector(v, audio);
    for (idct, pix_fmt) in [(Idct::Hq, "yuv420p10le"), (Idct::Accurate, "yuv420p")] {
        let file = dir.join(format!("archive-{pix_fmt}.mkv"));
        let r = export_stream(
            src,
            v,
            audio,
            &options(Preset::Archive, idct, (1, 1)),
            &file,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(r.frames_written, v.frames.len());
        assert_eq!(stream_field(&file, "v", "codec_name"), "ffv1");
        check_tags(&file, pix_fmt, "center");
        check_video_times(&file, v, base, MKV_TOLERANCE);

        let back = decoded_back(
            &file,
            "0:v",
            &[
                "-f",
                "rawvideo",
                "-fps_mode",
                "passthrough",
                "-pix_fmt",
                pix_fmt,
            ],
        );
        assert!(
            back == expected_video(src, v, idct),
            "{file:?} doesn't decode back to the same video"
        );
        for (i, a) in audio.iter().enumerate() {
            let back = decoded_back(&file, &format!("0:a:{i}"), &["-f", "s16le"]);
            assert!(
                back == expected_audio(src, a),
                "{file:?} doesn't decode back to the same audio"
            );
            assert_eq!(
                stream_field(&file, "a", "sample_rate"),
                a.format.sample_rate.to_string()
            );
            assert_eq!(stream_field(&file, "a", "codec_name"), "flac");
            check_audio_start(&file, v, a, base, MKV_TOLERANCE);
        }
    }
}

#[test]
fn archive_is_lossless_with_sector_timestamps() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    assert_eq!(v.frames.len(), GAPS.len() + 1);
    assert!(!v.timing.is_constant(), "the test stream should be VFR");
    let dir = tempfile::tempdir().unwrap();
    check_archive(&mut disc, v, &audio_of(&idx, v), dir.path());
}

fn check_lossy(src: &mut dyn SectorSource, v: &VideoStream, audio: &[&AudioStream], dir: &Path) {
    let base = base_sector(v, audio);
    let opts = options(Preset::H264, Idct::Hq, (1, 1));
    let file = dir.join("h264.mp4");
    export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
    assert_eq!(stream_field(&file, "v", "codec_name"), "h264");
    assert_eq!(stream_field(&file, "a", "codec_name"), "aac");
    assert_eq!(stream_field(&file, "a", "sample_rate"), "48000");
    // ffprobe's name for full-range 8-bit H.264.
    check_tags(&file, "yuvj420p", "center");
    check_video_times(&file, v, base, MP4_TOLERANCE);
    check_audio_lag(src, &file, v, audio[0]);

    for (ext, tolerance) in [("mkv", MKV_TOLERANCE), ("mp4", MP4_TOLERANCE)] {
        let mut opts = options(Preset::Av1, Idct::Hq, (1, 1));
        opts.audio = Some(AudioCodec::Opus);
        let file = dir.join(format!("opus.{ext}"));
        export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
        assert_eq!(stream_field(&file, "a", "codec_name"), "opus");
        assert_eq!(stream_field(&file, "a", "sample_rate"), "48000");
        check_video_times(&file, v, base, tolerance);
        check_audio_lag(src, &file, v, audio[0]);
    }

    let opts = options(Preset::Av1, Idct::Hq, (1, 1));
    let file = dir.join("av1.mkv");
    export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
    assert_eq!(stream_field(&file, "v", "codec_name"), "av1");
    check_tags(&file, "yuv420p10le", "center");
    check_video_times(&file, v, base, MKV_TOLERANCE);
    check_audio_start(&file, v, audio[0], base, MKV_TOLERANCE);

    // MP4 is the exact one, but it can't hold AV1's chroma siting, so that isn't checked.
    let file = dir.join("av1.mp4");
    export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
    assert_eq!(stream_field(&file, "v", "pix_fmt"), "yuv420p10le");
    assert_eq!(stream_field(&file, "v", "color_range"), "pc");
    check_video_times(&file, v, base, MP4_TOLERANCE);
    check_audio_start(&file, v, audio[0], base, MP4_TOLERANCE);
}

#[test]
fn lossy_presets_keep_timestamps_and_tags() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    check_lossy(&mut disc, v, &audio_of(&idx, v), dir.path());
}

/// Constant frame rate output: evenly spaced timestamps, the right picture on each
/// tick, and audio left exactly as it was.
fn check_cfr(src: &mut dyn SectorSource, v: &VideoStream, audio: &[&AudioStream], dir: &Path) {
    let base = base_sector(v, audio);
    let rate = constant_rate(v);
    let mut opts = options(Preset::Archive, Idct::Hq, (1, 1));
    opts.cfr = true;
    let file = dir.join("cfr.mkv");
    let r = export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
    assert_eq!(r.constant_rate, Some(rate));
    check_tags(&file, "yuv420p10le", "center");

    let want = expected_cfr_video(src, v, base, rate, Idct::Hq);
    let back = decoded_back(
        &file,
        "0:v",
        &[
            "-f",
            "rawvideo",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "yuv420p10le",
        ],
    );
    // The last frame's length isn't known, so FFmpeg's fps filter may hold it for
    // one more tick. Nothing else may differ.
    let (w, h) = (v.width as usize, v.height as usize);
    let frame = 2 * (w * h + 2 * w.div_ceil(2) * h.div_ceil(2));
    let extra = back.len().checked_sub(want.len()).unwrap_or_else(|| {
        panic!("{file:?}: {} bytes, wanted {}", back.len(), want.len());
    });
    assert!(
        extra == 0 || extra == frame,
        "{file:?}: {extra} bytes extra"
    );
    assert!(
        back[..want.len()] == want[..],
        "{file:?} has the wrong picture on some tick"
    );
    assert!(
        back[want.len()..] == want[want.len() - extra..],
        "{file:?} ends on a different picture"
    );

    let times = packet_times(&file, "v");
    let period = rate.1 as f64 / rate.0 as f64;
    for (k, t) in times.iter().enumerate() {
        let want = k as f64 * period;
        assert!(
            (t - want).abs() <= MKV_TOLERANCE,
            "tick {k} of {file:?}: at {t}, wanted {want}"
        );
    }
    for (i, a) in audio.iter().enumerate() {
        let back = decoded_back(&file, &format!("0:a:{i}"), &["-f", "s16le"]);
        assert!(back == expected_audio(src, a), "{file:?}: audio changed");
        check_audio_start(&file, v, a, base, MKV_TOLERANCE);
    }

    // MP4 and a lossy preset, where the rate also has to reach the encoder.
    let mut opts = options(Preset::H264, Idct::Hq, (1, 1));
    opts.cfr = true;
    let file = dir.join("cfr.mp4");
    export_stream(src, v, audio, &opts, &file, |_, _| {}).unwrap();
    let got = stream_field(&file, "v", "avg_frame_rate");
    assert_eq!(got, format!("{}/{}", rate.0, rate.1), "{file:?}");
    let times = packet_times(&file, "v");
    for (k, t) in times.iter().enumerate() {
        let want = k as f64 * period;
        assert!(
            (t - want).abs() <= MP4_TOLERANCE,
            "tick {k} of {file:?}: at {t}, wanted {want}"
        );
    }
    if let Some(a) = audio.first() {
        check_audio_lag(src, &file, v, a);
    }
}

#[test]
fn cfr_is_evenly_spaced_and_keeps_the_picture_in_sync() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    check_cfr(&mut disc, v, &audio_of(&idx, v), dir.path());
}

#[test]
fn archive_refuses_lossy_audio() {
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(Preset::Archive, Idct::Hq, (1, 1));
    opts.audio = Some(AudioCodec::Opus);
    let file = dir.path().join("x.mkv");
    let err = export_stream(&mut disc, v, &[], &opts, &file, |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
}

#[test]
fn sar_reaches_the_output() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sar.mkv");
    let opts = options(Preset::Archive, Idct::Hq, (5, 4));
    export_stream(&mut disc, v, &audio_of(&idx, v), &opts, &file, |_, _| {}).unwrap();
    assert_eq!(stream_field(&file, "v", "sample_aspect_ratio"), "5:4");
}

#[test]
fn stream_without_audio_exports() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("silent.mkv");
    let opts = options(Preset::Archive, Idct::Hq, (1, 1));
    let r = export_stream(&mut disc, v, &[], &opts, &file, |_, _| {}).unwrap();
    assert_eq!(r.audio_packets, 0);
    assert_eq!(stream_field(&file, "a", "codec_name"), "");
    check_video_times(&file, v, base_sector(v, &[]), MKV_TOLERANCE);

    // A preset with audio options of its own shouldn't draw warnings about them.
    let file = dir.path().join("silent.mp4");
    let opts = options(Preset::H264, Idct::Hq, (1, 1));
    let r = export_stream(&mut disc, v, &[], &opts, &file, |_, _| {}).unwrap();
    assert_eq!(r.warnings, Vec::<String>::new());
}

#[test]
fn ffmpeg_failure_is_reported_and_leaves_nothing_behind() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    // A folder that doesn't exist: FFmpeg quits while we're still writing to it.
    let file = dir.path().join("no such folder").join("x.mkv");
    let opts = options(Preset::Archive, Idct::Hq, (1, 1));
    let err = export_stream(&mut disc, v, &audio_of(&idx, v), &opts, &file, |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::FfmpegFailed { .. }), "{err}");
}

#[test]
fn failed_export_keeps_the_previous_file() {
    if !have_ffmpeg() {
        return;
    }
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let v = &idx.video[0];
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.mkv");
    std::fs::write(&file, b"keep me").unwrap();
    // Every frame fails to decode, once the frame data has been zeroed out.
    let mut broken = v.clone();
    for f in &mut broken.frames {
        f.number += 1000;
    }
    let opts = options(Preset::Archive, Idct::Hq, (1, 1));
    let err = export_stream(&mut disc, &broken, &[], &opts, &file, |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::NoFrames(_)), "{err}");
    assert_eq!(std::fs::read(&file).unwrap(), b"keep me");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn missing_ffmpeg_is_a_clear_error() {
    let mut disc = synthetic_disc();
    let idx = index_of(&mut disc);
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(Preset::Archive, Idct::Hq, (1, 1));
    opts.ffmpeg = dir.path().join("not-ffmpeg");
    let file = dir.path().join("x.mkv");
    let err = export_stream(&mut disc, &idx.video[0], &[], &opts, &file, |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::FfmpegStart { .. }), "{err}");
}

/// The gated run over real discs, which never ends up in CI.
#[test]
fn real_discs() {
    let Some(folder) = std::env::var_os("PSXVC_TEST_DISCS") else {
        return;
    };
    assert!(have_ffmpeg(), "PSXVC_TEST_DISCS needs ffmpeg and ffprobe");
    let mut cues: Vec<PathBuf> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "cue"))
        .collect();
    cues.sort();
    assert!(!cues.is_empty(), "no .cue files in {folder:?}");

    for cue in cues {
        let mut disc = BinCueDisc::open(&cue).unwrap();
        let idx = index_of(&mut disc);
        for v in &idx.video {
            eprintln!(
                "{}: stream {} ({} frames)",
                cue.display(),
                v.id,
                v.frames.len()
            );
            let dir = tempfile::tempdir().unwrap();
            check_archive(&mut disc, v, &audio_of(&idx, v), dir.path());
            let (n, d) = constant_rate(v);
            eprintln!("  constant rate {n}/{d}, average {:.3} fps", v.timing.fps);
            check_cfr(&mut disc, v, &audio_of(&idx, v), dir.path());
        }
        // Lossy encoding takes longer, so these get the shortest stream with audio.
        let v = idx
            .video
            .iter()
            .filter(|v| !v.audio.is_empty())
            .min_by_key(|v| v.frames.len())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        check_lossy(&mut disc, v, &audio_of(&idx, v), dir.path());
    }
}
