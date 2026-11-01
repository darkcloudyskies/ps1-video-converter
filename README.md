# ps1-video-converter (`psxvc`)

A Rust tool for pulling PlayStation 1 FMV (STR video + XA audio) off raw disc
images and into modern codecs and containers at the highest possible quality.
It is a ground-up rewrite inspired by [jPSXdec](https://github.com/m35/jpsxdec),
with a different focus:

- decode straight to YCbCr 4:2:0 (no RGB round trip), at 10-bit precision
- correct colour tags (BT.601, full range, centre chroma siting)
- exact per-frame timestamps from sector positions (variable frame rate)
  instead of duplicating frames
- native-rate lossless audio (FLAC at 37.8/18.9 kHz)
- lossless FFV1/MKV archive preset, plus AV1/H.264 presets

See [docs/PLAN.md](docs/PLAN.md) for the full design and roadmap.

## Status

Reading the disc, finding the streams, decoding them (STRv2 and STRv3 video, XA
audio) and exporting them are in place. What's still open in export is listed in
[docs/export-notes.md](docs/export-notes.md).

`psxvc export` needs FFmpeg with the codecs for its preset. It looks beside `psxvc`,
then on PATH, or you can name one with `--ffmpeg`.

| Command | What it does |
|---|---|
| `psxvc scan game.cue` | Scans every sector, finds STR video and XA audio streams, writes `game.psxidx.json` |
| `psxvc list game.psxidx.json [--frames ID]` | Prints the streams (and optionally per-frame sector info) |
| `psxvc extract-sectors game.cue --video ID -o clip.bin` | Copies one video stream and its audio into a small standalone BIN/CUE for debugging |
| `psxvc export game.cue --stream ID [--preset archive\|h264\|av1] [--idct hq\|accurate] [-o out.mkv]` | Encodes a video stream and its audio, keeping exact frame timing. `--all` exports every stream, with `-o` as the folder |
| `psxvc frame game.cue --stream ID --frame N [--idct hq\|accurate] -o f.png` | Decodes one video frame to a PNG, as a quick visual check |
| `psxvc audio game.cue --stream ID -o a.wav` | Decodes an audio stream to a WAV at its native rate |
| `psxvc frame-hashes game.cue --stream ID` | Prints a checksum per frame, for comparing with jPSXdec |

Input must be a **raw BIN/CUE** rip (2352-byte sectors). A 2048-byte `.iso`
throws away XA audio and the sector subheaders, so it can't be used.

## Building

```bash
cargo build --release
```

The binary is `target/release/psxvc`.

## Testing

```bash
cargo test --workspace
```

The tests use synthetic sectors and frames generated in code, so they need no game
data. Export tests run through a real FFmpeg and skip themselves if it isn't on PATH,
which is the case on CI. Testing against real discs runs locally (see the testing
section of `docs/PLAN.md`, [tools/jpsxdec-oracle](tools/jpsxdec-oracle/README.md) and
[the export tests](crates/psx-export/tests/ffmpeg.rs)).
