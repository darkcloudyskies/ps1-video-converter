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

Reading the disc, finding the streams and decoding them are in place (STRv2 and
STRv3 video, XA audio). Export to modern codecs and containers comes next (see
[docs/export-notes.md](docs/export-notes.md)).

| Command | What it does |
|---|---|
| `psxvc scan game.cue` | Scans every sector, finds STR video and XA audio streams, writes `game.psxidx.json` |
| `psxvc list game.psxidx.json [--frames ID]` | Prints the streams (and optionally per-frame sector info) |
| `psxvc extract-sectors game.cue --video ID -o clip.bin` | Copies one video stream and its audio into a small standalone BIN/CUE for debugging |
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
data. Testing against real discs runs locally (see the testing section of
`docs/PLAN.md` and [tools/jpsxdec-oracle](tools/jpsxdec-oracle/README.md)).
