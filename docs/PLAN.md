# PS1 Video Converter — Rust rewrite of jPSXdec with a quality-first export path

## Context

jPSXdec (Java, m35/jpsxdec) is the reference tool for pulling FMV out of PS1 discs, but its output pipeline is dated: it converts to RGB, writes AVI (MJPG/uncompressed) or image sequences, resamples audio, and duplicates frames to fake constant frame rates. This project rebuilds it in Rust with one main goal: **get the original video and audio into modern codecs and containers with as little loss as possible.**

Decisions: **Rust**, **CLI first with a GUI later**, **BIN/CUE input first**, **a self-contained final GUI app that includes everything it needs**, **FFmpeg run as a separate program rather than linked** (see `docs/export-notes.md`), and **Parasite Eve, Parasite Eve II and Koudelka** as the target games, with other formats added later.

## Status

| Phase | State |
|---|---|
| 1. Foundations | **Done**: `psx-disc` (BIN/CUE, sectors, EDC, ISO9660), `psx-index` (STR/XA stream indexer), `psxvc scan` / `list` / `extract-sectors`, CI |
| Step 0: confirm target-game formats | **In progress**: 3 of 8 discs scanned and compared with jPSXdec; results and open issues in `docs/target-games.md` |
| 2. Decode MVP | **Done**: `psx-mdec` (STRv2/v3, `accurate` and `hq` IDCT), `psx-xa`, `psx-stream`, `psxvc frame` / `audio` / `frame-hashes`. Matches jPSXdec on all 3 scanned discs: see `docs/target-games.md` |
| 3. Export | **In progress**: `psx-export` and `psxvc export` work, with `archive`, `h264` and `av1` presets, checked on all 3 scanned discs. Still open: `--cfr`, Opus, player testing. See `docs/export-notes.md` |
| 4–6 | Not started |

Keep this table up to date as phases land.

## Quality principles (what we do differently from jPSXdec)

1. **Stay in YCbCr.** MDEC data is natively YCbCr 4:2:0 (16×16 macroblocks, 8×8 DCT). Run the IDCT and hand the Y/Cb/Cr planes straight to the encoder as `yuv420p`/`yuv420p10`. No RGB round trip and no chroma resampling.
2. **Correct colour metadata.** Tag BT.601 matrix (`smpte170m`), **full range** (PS1 MDEC uses full-range YCbCr), and MPEG-1 style *centre* chroma siting. Check this against the MDEC's own RGB output on known frames.
3. **Choice of IDCT.**
   - `accurate`: bit-exact emulation of the PS1 MDEC's fixed-point IDCT, which reproduces what the console showed.
   - `hq` (default): double-precision IDCT with no intermediate clamping, output at 10-bit, so rounding errors don't add banding.
4. **Exact timing instead of duplicated frames.** Give each frame a timestamp based on its sector position (75 sectors/s at 1×, 150 at 2×) and use a timebase like 1/150 s or 1/37800 s. Write variable frame rate output into MKV/MP4. FFmpeg's MKV writer rounds timestamps to 1 ms, so MP4 is the only one of the two that keeps them exact. A `--cfr` option can still produce constant frame rate output if needed.
5. **Native audio.** Decode XA ADPCM to PCM at its native rate (37 800 or 18 900 Hz, mono or stereo) and store it losslessly as FLAC. Only resample when the user picks a lossy target that requires it (e.g. Opus at 48 kHz).
6. **Pixel aspect, not scaling.** Crop to the real frame dimensions and set the SAR/DAR (usually 4:3) in the container, instead of resizing pixels.
7. **Lossless archival first.** Default output is FFV1 (10-bit) + FLAC in MKV. Distribution presets: AV1 (SVT-AV1 10-bit) + Opus/FLAC in MKV or MP4, and x264/x265 for compatibility. Upscaling and deblocking are optional, explicit filters and never on by default.

## Architecture (Cargo workspace)

```
crates/
  psx-disc/     sector reading: BIN/CUE parser, raw 2352-byte Mode2 Form1/Form2, subheaders, EDC/ECC check
  psx-index/    scan disc → find ISO9660 files + raw streams; identify video/audio/TIM; serialize index (JSON)
  psx-mdec/     bitstream decoders (STRv2, STRv3, Iki, …) → MDEC codes → IDCT (accurate | hq) → YCbCr planes
  psx-xa/       XA ADPCM (+ SPU ADPCM) decoder, interleave/channel demux
  psx-stream/   demux interleaved sectors → frames (multi-sector reassembly) + audio packets with sector timestamps
  psx-export/   encode/mux by piping a NUT stream to an ffmpeg process, presets, colour metadata, VFR timestamps
  psxvc-cli/    `psxvc` binary (clap)
  (later) psxvc-gui/  Tauri app reusing the library crates
```

Key external crates: `clap`, `png` for frame dumps, `rayon` to decode frames in parallel, `serde`/`serde_json` for the index, `thiserror`/`anyhow`, `indicatif` for progress bars. CHD support (`chd` crate) can slot into `psx-disc` later behind a `SectorSource` trait.

### Core data flow
`SectorSource` (BIN/CUE) → `Indexer` (spots `0x80010160` video sector headers and XA subheaders by file/channel) → `StreamDemuxer` (reassembles frame chunks, tracks sector numbers) → `FrameDecoder` (bitstream → coefficients → IDCT → planes) + `XaDecoder` → `Exporter` (NUT stream to an `ffmpeg` process, which encodes and muxes, with PTS taken from sector positions).

## Target games: Parasite Eve, Parasite Eve II, Koudelka

These three are the first acceptance targets, and phase priority follows them.
- **Step 0 (before writing decoders):** run jPSXdec's indexer (`jpsxdec -f disc.bin -x disc.idx`) on every disc of all three games: PE1 ×2, PE2 ×2, Koudelka ×4. The `.idx` lists the exact video variant, dimensions, sectors/frame and audio format of each stream, so we confirm the formats instead of guessing. Save these index summaries (metadata only, no game data) as `docs/target-games.md`.
- I expect all three to use Sony's standard STR (v2/v3) video with XA audio, but that's unconfirmed until Step 0. If any title turns out to use a custom variant, that variant moves into phase 1.
- Multi-disc handling: an index per disc, and output names like `PE2-D1-<file>-<stream>.mkv`.
- These discs also serve as the gated real-disc regression set: the decode must match jPSXdec on every stream across all 8 discs.

## Format coverage roadmap

| Phase | Video | Audio |
|---|---|---|
| 1 | STRv2 (most common), STRv3 | XA ADPCM 4-bit, mono/stereo, 37.8/18.9 kHz |
| 2 | STRv1, FF7 (camera-data frames), FF8/FF9 variants, Iki | XA 8-bit, SPU-ADPCM streams |
| 3 | Lain, Crusader, Ace Combat 3, Aconcagua, other one-offs | game-specific audio |

jPSXdec's source and docs (`jpsxdec/modules/*`, its PlayStation1_STR_format.txt) are the spec reference for the bitstream variants. We use them as documentation and test oracles and don't copy code: jPSXdec's licence is its own non-commercial one, with conditions on derivative works. `tools/jpsxdec-oracle` calls its jar at run time and includes none of its code.

## CLI shape

```
psxvc scan  game.cue              # writes game.psxidx.json, lists streams
psxvc list  game.psxidx.json      # id, type, WxH, frames, fps, audio
psxvc export game.cue --stream 3 --preset archive|av1|h264 [--idct accurate|hq] [--sar W:H] [--cfr, not built yet] [--ffmpeg path] [-o out.mkv]
psxvc export game.cue --all --preset av1 -o out/
psxvc frame game.cue --stream 3 --frame 120 -o f.png   # quick visual check
psxvc audio game.cue --stream 3 -o a.wav               # native-rate WAV dump
```

`export` looks for FFmpeg in this order: `--ffmpeg`, beside the `psxvc` program, then PATH.

## Phases

1. **Foundations** (done): workspace, CI (fmt/clippy/test), CUE/BIN reader, sector parsing, disc indexer for STR/XA.
2. **Decode MVP** (done): STRv2/v3 bitstream, both IDCTs, XA decoder, PNG frame dump and WAV dump.
3. **Export**: `psx-export`, FFV1/FLAC MKV archive preset, AV1/H.264 presets, VFR timestamps, colour/SAR metadata, A/V sync. See `docs/export-notes.md`.
4. **Coverage**: the phase 2 and 3 formats above, broken-frame handling (skip or repeat with a warning).
5. **GUI**: Tauri app with disc browser, frame scrubber, side-by-side `accurate` vs `hq` comparison, batch queue. FFmpeg ships inside the app as a Tauri sidecar, in a build trimmed to the codecs the presets need; see the licensing notes in `docs/export-notes.md`.
6. **Optional filters**: deblock, deband, and upscaling hooks (e.g. external model via ffmpeg filter).

## Where development and testing happen (local)

Development and testing both happen on the owner's Windows machine, with the real discs in `discs/` (ignored by git). GitHub CI still runs fmt, clippy and the tests that need no game data.

- **Tests that can be committed:** test files that are legal to commit:
  - Build test streams with **psxavenc**, the open-source PS1 STR/XA encoder from PSn00bSDK, from frames and audio we create, e.g. ffmpeg `testsrc`, colour bars and gradients. Gradients are good at exposing banding and chroma problems. Because we know the source picture, we can measure quality (PSNR/SSIM), not only check that output matches.
  - Run **jPSXdec** (Java) as the reference decoder for the same test files.
  - Wrap the generated streams in a minimal BIN/CUE so the full disc → export pipeline gets tested.
- **Real discs:** the full regression run on the 8 real discs. A `psxvc verify disc.cue --against disc.idx` command (or `cargo test` with `PSXVC_TEST_DISCS`) compares our output with jPSXdec's and writes a short text/JSON report. Until `verify` exists, `tools/jpsxdec-oracle/compare.ps1` does the decode comparison, stream by stream, and `psxvc scan` summaries plus jPSXdec `.idx` files cover the index.
- **Debugging clips:** `psxvc extract-sectors` cuts one stream (a few MB) into a small BIN/CUE. These contain game data, so never commit them. Keep them out of `tests/fixtures/`, where `.gitignore` allows `.bin` and `.cue` files.

**Tool status:** Rust, Java 21 and FFmpeg 9 are installed. jPSXdec is a sibling checkout at `../jpsxdec`, with a built jar at `jpsxdec/_ant/release/jpsxdec.jar`. psxavenc still needs installing. `git` isn't on PATH; use the copy bundled with GitHub Desktop. Phase 3 doesn't need `clang`, `pkg-config` or FFmpeg's development files, but they're installed (with a shared FFmpeg build) in case linking is ever revisited.

## Verification

- **Unit tests**: bit reader, VLC tables, IDCT against reference vectors, XA decoder against known PCM.
- **Golden tests against jPSXdec**: on sample sectors, compare MDEC coefficients exactly, and compare `accurate` IDCT output exactly with jPSXdec's "PSX accurate" decode. The `hq` output must be within ±1 per sample of a float reference. Small synthetic and homebrew STR fixtures go in the repo. Tests on real game discs live behind an env var (`PSXVC_TEST_DISCS=...`) because commercial data can't be committed.
- **End-to-end**: `psxvc export --preset archive` then `ffprobe` checks codec, `yuv420p10le`, `color_range=pc`, `colorspace=smpte170m`, `chroma_location=center`, SAR, and an audio sample rate of 37800. Compare every frame's PTS with its sector position, because FFmpeg can flatten variable frame rate without any warning and the lossless check won't notice. Decode the FFV1 output back and compare it with the frame dump to confirm it's lossless.
- **A/V sync**: test stream with known audio/video sector interleave, with a tolerance check on PTS.
