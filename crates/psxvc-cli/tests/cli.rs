//! End-to-end: build a synthetic BIN/CUE, then run the commands over it.

use psx_disc::iso9660::testutil::tiny_disc;
use psx_disc::{RawSector, SubHeader, SubMode};
use psx_index::video::StrHeader;
use psx_mdec::testutil::encode_frame;
use psx_mdec::Block;
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn write_disc(dir: &Path) -> std::path::PathBuf {
    let mut disc = tiny_disc(80);
    let mut frame = 1;
    let mut chunk = 0;
    for lba in 30..70u32 {
        disc.sectors[lba as usize] = if (lba - 30) % 8 == 7 {
            let sh = SubHeader {
                file: 1,
                channel: 0,
                submode: SubMode(SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME),
                coding_info: 0x01,
            };
            RawSector::build_mode2(lba, sh, &[0u8; 2304])
        } else {
            let h = StrHeader {
                chunk_number: chunk,
                chunk_count: 5,
                frame_number: frame,
                demux_size: 10_000,
                width: 320,
                height: 240,
                mdec_code_count: 1000,
                magic_3800: 0x3800,
                quant_scale: 4,
                version: 3,
            };
            let mut data = vec![0u8; 2048];
            data[..32].copy_from_slice(&h.to_bytes());
            chunk += 1;
            if chunk == 5 {
                chunk = 0;
                frame += 1;
            }
            let sh = SubHeader {
                file: 1,
                channel: 1,
                submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
                coding_info: 0,
            };
            RawSector::build_mode2(lba, sh, &data)
        };
    }
    let bin = dir.join("Test Disc.bin");
    let mut f = std::fs::File::create(&bin).unwrap();
    for s in &disc.sectors {
        f.write_all(&s.bytes[..]).unwrap();
    }
    let cue = dir.join("Test Disc.cue");
    std::fs::write(
        &cue,
        "FILE \"Test Disc.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    cue
}

fn psxvc(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_psxvc"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "psxvc {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A disc with one real 32×16 frame: left half dark, right half light. It's in a
/// single sector, so it's small enough to fit one chunk. Audio follows, with a
/// constant ADPCM value.
fn write_decodable_disc(dir: &Path, version: u16) -> std::path::PathBuf {
    let mut blocks = Vec::new();
    for mb in 0..2 {
        for b in 0..6 {
            let mut coefs = [0i16; 64];
            // Y blocks only; chroma stays neutral. DC ±200 → sample ±50.
            if b >= 2 {
                coefs[0] = if mb == 0 { -200 } else { 200 };
            }
            blocks.push(Block { qscale: 1, coefs });
        }
    }
    let frame = encode_frame(&blocks, 1, version);

    let mut disc = tiny_disc(60);
    let h = StrHeader {
        chunk_number: 0,
        chunk_count: 1,
        frame_number: 1,
        demux_size: frame.len() as u32,
        width: 32,
        height: 16,
        mdec_code_count: u16::from_le_bytes([frame[0], frame[1]]),
        magic_3800: 0x3800,
        quant_scale: 1,
        version,
    };
    let mut data = vec![0u8; 2048];
    data[..32].copy_from_slice(&h.to_bytes());
    data[32..32 + frame.len()].copy_from_slice(&frame);
    let video = SubHeader {
        file: 1,
        channel: 1,
        submode: SubMode(SubMode::DATA | SubMode::REAL_TIME),
        coding_info: 0,
    };
    disc.sectors[30] = RawSector::build_mode2(30, video, &data);

    // Range 8, filter 0: each byte 0x11 gives nibbles worth 0x1000 >> 8 = 16.
    let mut pcm = vec![0x11u8; 2304];
    for group in pcm.chunks_exact_mut(128) {
        group[..16].fill(0x08);
    }
    let audio = SubHeader {
        file: 1,
        channel: 0,
        submode: SubMode(SubMode::AUDIO | SubMode::FORM2 | SubMode::REAL_TIME),
        coding_info: 0x01,
    };
    for lba in [31, 39] {
        disc.sectors[lba as usize] = RawSector::build_mode2(lba, audio, &pcm);
    }

    let bin = dir.join("d.bin");
    let mut f = std::fs::File::create(&bin).unwrap();
    for s in &disc.sectors {
        f.write_all(&s.bytes[..]).unwrap();
    }
    bin
}

#[test]
fn frame_and_audio_export() {
    for version in [2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_decodable_disc(dir.path(), version);
        let bin_s = bin.to_str().unwrap();
        let out = psxvc(&["scan", "--quiet", bin_s]);
        assert!(out.contains("32x16"), "{out}");

        for idct in ["hq", "accurate"] {
            let png_path = dir.path().join(format!("{idct}.png"));
            psxvc(&[
                "frame",
                bin_s,
                "--stream",
                "0",
                "--frame",
                "1",
                "--idct",
                idct,
                "-o",
                png_path.to_str().unwrap(),
            ]);
            let decoder = png::Decoder::new(std::fs::File::open(&png_path).unwrap());
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut pixels).unwrap();
            assert_eq!((info.width, info.height), (32, 16));
            assert_eq!(info.color_type, png::ColorType::Rgb);
            assert_eq!(&pixels[..3], &[78, 78, 78], "{idct} left half");
            assert_eq!(
                &pixels[16 * 3..16 * 3 + 3],
                &[178, 178, 178],
                "{idct} right"
            );
        }

        let wav = dir.path().join("a.wav");
        let out = psxvc(&["audio", bin_s, "--stream", "0", "-o", wav.to_str().unwrap()]);
        assert!(out.contains("2 sectors"), "{out}");
        let b = std::fs::read(&wav).unwrap();
        assert_eq!(b.len(), 44 + 2 * 2016 * 2 * 2);
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 37_800);
        assert_eq!(i16::from_le_bytes([b[44], b[45]]), 16);

        let hashes = psxvc(&["frame-hashes", bin_s, "--stream", "0"]);
        assert_eq!(hashes.lines().count(), 1);
        assert!(
            hashes.starts_with("1 ") && !hashes.contains("error"),
            "{hashes}"
        );
    }
}

#[test]
fn export_writes_a_file_and_names_it_after_the_stream() {
    let have_ffmpeg = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !have_ffmpeg {
        eprintln!("skipping: ffmpeg isn't on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = write_decodable_disc(dir.path(), 2);
    let bin_s = bin.to_str().unwrap();
    psxvc(&["scan", "--quiet", bin_s]);

    let named = dir.path().join("named.mkv");
    let out = psxvc(&[
        "export",
        bin_s,
        "--stream",
        "0",
        "--quiet",
        "-o",
        named.to_str().unwrap(),
    ]);
    assert!(out.contains("1 of 1 frames"), "{out}");
    assert!(named.exists());

    let all = dir.path().join("all");
    psxvc(&[
        "export",
        bin_s,
        "--all",
        "--quiet",
        "-o",
        all.to_str().unwrap(),
    ]);
    let names: Vec<String> = std::fs::read_dir(&all)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["d-MOVIE-OPEN-0.mkv"]);
}

#[test]
fn export_rejects_bad_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_decodable_disc(dir.path(), 2);
    let bin_s = bin.to_str().unwrap();
    psxvc(&["scan", "--quiet", bin_s]);
    for args in [
        vec!["export", bin_s],
        vec!["export", bin_s, "--stream", "9"],
        vec!["export", bin_s, "--stream", "0", "--sar", "4"],
        vec!["export", bin_s, "--stream", "0", "--sar", "0:1"],
        vec!["export", bin_s, "--stream", "0", "-o", "x.mp4"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_psxvc"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
    }
}

#[test]
fn missing_stream_or_frame_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_decodable_disc(dir.path(), 2);
    let bin_s = bin.to_str().unwrap();
    psxvc(&["scan", "--quiet", bin_s]);
    let out = dir.path().join("x.png");
    for (stream, frame) in [("5", "1"), ("0", "99")] {
        let status = Command::new(env!("CARGO_BIN_EXE_psxvc"))
            .args(["frame", bin_s, "--stream", stream, "--frame", frame, "-o"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(!status.status.success());
    }
}

#[test]
fn scan_list_extract() {
    let dir = tempfile::tempdir().unwrap();
    let cue = write_disc(dir.path());
    let cue_s = cue.to_str().unwrap();

    let out = psxvc(&["scan", "--quiet", cue_s]);
    assert!(out.contains("/MOVIE/OPEN.STR"), "{out}");
    assert!(out.contains("STRv3"), "{out}");
    assert!(out.contains("320x240"), "{out}");
    let idx_path = dir.path().join("Test Disc.psxidx.json");
    assert!(idx_path.exists());

    let out = psxvc(&["list", idx_path.to_str().unwrap(), "--frames", "0"]);
    assert!(out.contains("Video streams: 1"), "{out}");
    assert!(out.contains("Audio streams: 1"), "{out}");
    assert!(out.contains("37800 Hz stereo 4-bit"), "{out}");

    let ex = dir.path().join("excerpt.bin");
    psxvc(&[
        "extract-sectors",
        cue_s,
        "--video",
        "0",
        "--index",
        idx_path.to_str().unwrap(),
        "-o",
        ex.to_str().unwrap(),
    ]);
    // Stream spans 30..69; padded by 16 and clamped to the disc (0..80).
    assert_eq!(std::fs::metadata(&ex).unwrap().len(), (79 - 14 + 1) * 2352);

    // The excerpt has no filesystem but still scans to the same streams.
    let out = psxvc(&[
        "scan",
        "--quiet",
        ex.with_extension("cue").to_str().unwrap(),
    ]);
    assert!(out.contains("Video streams: 1"), "{out}");
    assert!(out.contains("Audio streams: 1"), "{out}");
}
