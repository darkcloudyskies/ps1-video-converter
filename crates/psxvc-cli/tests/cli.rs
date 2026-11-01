//! End-to-end: build a synthetic BIN/CUE, scan it, list it, extract a stream.

use psx_disc::iso9660::testutil::tiny_disc;
use psx_disc::{RawSector, SubHeader, SubMode};
use psx_index::video::StrHeader;
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
