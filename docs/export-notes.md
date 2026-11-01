# Export notes

`psx-export` encodes by piping decoded frames and audio to an `ffmpeg` process. This
records why it works that way, the details that are easy to get wrong, and what's
still open in Phase 3.

## Why a pipe instead of `rsmpeg`

The final GUI app has to include everything it needs, and that favours shipping
`ffmpeg.exe` beside the app (a Tauri sidecar) over linking FFmpeg:

- **Build:** it needs only `cargo build`, with no clang or FFmpeg development
  files on any build machine, including CI.
- **Versions:** when checked (November 2026), the latest `rsmpeg` was
  `0.18.0+ffmpeg.8.0`, a major version behind the installed FFmpeg 9.0.2. The
  bindings were never test-built against FFmpeg 9.
- **Licensing:** see below.

FFmpeg's command line and process handling all live in
[ffmpeg.rs](../crates/psx-export/src/ffmpeg.rs), so the rest of the code doesn't
depend on how it's run.

## How the pipe works

`psxvc` writes one NUT stream to `ffmpeg`'s stdin, holding both video and audio.
Using one pipe avoids juggling two on Windows, and NUT stores exact rational
timestamps per stream, which raw video pipes and Y4M can't.

- **Video with `hq`:** `yuv420p10le`, tag `Y3 0B 0A` (bytes `59 33 0B 0A`).
- **Video with `accurate`:** `yuv420p`, tag `I420`, so the console's 8-bit values go
  through unscaled.
- **Video timing:** time base 1/(sectors per second). A frame's PTS is its `last_lba`
  minus the base sector.
- **Audio:** `pcm_s16le`, tag `PSD 0x10`, time base 1/(sample rate), one packet per
  sector. Its PTS is the sector's offset from the base sector, in samples:
  `(lba - base) * rate / sectors_per_second`.
- **Base sector:** the sector of the first packet, which is the lower of the first
  frame's `last_lba` and the audio's first sector. FFmpeg moves the earliest packet
  to zero anyway, so using anything else shifts every timestamp by a constant.
- **Order:** disc order, which is already correctly interleaved. Frames and audio are
  read as they're needed, so memory use doesn't grow with the stream.
- **10-bit samples:** full range, so luma is `(y + 128) * 1023 / 255` and chroma is
  `c * 1023 / 255 + 512`, rounded and clamped to 0..1023.
- **8-bit samples:** `y + 128` and `c + 128`, rounded and clamped to 0..255.

### NUT details that matter

[nut.rs](../crates/psx-export/src/nut.rs) shows all of this in about 150 lines.

- FFmpeg rejects frames with `header_idx invalid` unless the main header has the
  field that's optional in the spec, the elision header count. Write 0 there.
- Each frame gets a syncpoint before it. Each frame has `FLAG_CHECKSUM` set, because
  frames larger than `2 * max_distance` must have one.
- A packet's checksum covers the data after `forward_ptr`, using CRC-32 with
  polynomial `0x04C11DB7`, initial value 0, written big-endian.
- `ffmpeg` prints `Guessed Channel Layout: stereo`, which is harmless and filtered
  out, along with SVT-AV1's banner.

## FFmpeg arguments

```text
ffmpeg -y -color_range pc -f nut -i pipe:0 -map 0
  -fps_mode passthrough -enc_time_base:v 1:<sectors per second>
  -vf setparams=color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m:range=pc
  -color_primaries smpte170m -color_trc smpte170m -colorspace smpte170m
  -color_range pc -chroma_sample_location 2 -pix_fmt <yuv420p10le or yuv420p>
  <codec arguments> <output>
```

| Preset | Video | Audio | Container |
|---|---|---|---|
| `archive` | `-c:v ffv1 -level 3 -g 1 -slicecrc 1` | FLAC | MKV |
| `h264` | `-c:v libx264 -crf 14` | FLAC | MP4 (or MKV with `-o x.mkv`) |
| `av1` | `-c:v libsvtav1 -crf 20 -preset 6` | FLAC | MKV (or MP4 with `-o x.mp4`) |

MP4 output also needs `-video_track_timescale <sectors per second>`.

Three of these are easy to get wrong:

- **`-enc_time_base:v`:** without it, FFmpeg rounds PTS to the encoder's guessed
  frame rate and silently produces constant frame rate. The result still passes the
  lossless check, so tests must compare timestamps too.
- **`setparams`:** without it, primaries and transfer come out `unknown`.
- **`-color_range pc` before `-i`:** this applies to the input and is separate from
  the one on the output.

## Failures

The output is written as `<name>.partial.<ext>` and renamed once FFmpeg succeeds, so
a failed export can't replace a good file or leave half of one behind. FFmpeg's
output is read on its own thread so neither side can block on a full pipe. If FFmpeg
quits mid-stream, the error shows what it printed.

A frame that won't decode is skipped with a warning. That leaves a gap in the video
but doesn't move any other frame. Phase 4 decides on broken frames properly.

## Results

Checked by `cargo test --release -p psx-export` with `PSXVC_TEST_DISCS` set, on
Parasite Eve Disc 1, Parasite Eve II Disc 1 and Koudelka Disc 4. See the top of
[tests/ffmpeg.rs](../crates/psx-export/tests/ffmpeg.rs) for how to run it.

| Check | Streams covered | Result |
|---|---|---|
| FFV1 and FLAC decode back byte-identical, with both IDCTs | All 67 | Pass |
| Video timestamps within 1 ms of the sector position (MKV) | All 67 | Pass |
| `yuv420p10le` or `yuv420p`, full range, BT.601, centre chroma | All 67 | Pass |
| H.264 in MP4 and AV1 in MP4: timestamps within 0.001 ms | Shortest with audio on each disc | Pass |
| H.264 in MP4 and AV1 in MKV: tags as above | Shortest with audio on each disc | Pass |

Exporting the 36 streams of Parasite Eve II Disc 1 (19,183 frames, down to 48x32)
took 12 s and 0.72 GB with `archive`, 17 s and 154 MB with `h264`, and 24 s and
121 MB with `av1`, with no failures. The only warnings are from SVT-AV1 on the two
48x32 streams, where it turns off some of its tools because the frame is so small.
That's fast enough that decoding doesn't need `rayon` for now.

## Decisions

- **Exact timestamps in the archive:** MKV's 1 ms rounding is accepted. It comes from
  FFmpeg's MKV writer, which has no option to change it. It doesn't build up over a
  stream, and it's small against a frame every 60 ms or so. Anyone who needs exact
  timestamps can use `h264` or `av1` in MP4.
- **Aspect ratio:** pixels are tagged square by default, and `--sar W:H` overrides
  that. The shape of a pixel comes from the console's display mode, not from how
  much of the screen a stream fills. So 320x240 comes out at 4:3, and 320x192 comes
  out at 5:3, as a letterboxed picture would look on a TV. This assumes the game shows
  320-wide streams 1:1 in a 320-wide mode. One Parasite Eve II frame looks right
  this way, but nothing has compared it with a console.
- **Trailing audio:** it's kept. Audio often runs past the last frame, by up to about
  2 s on Parasite Eve Disc 1, and the file's length includes it.

## Still to do or decide

- **`--cfr`:** in the plan's CLI but not built.
- **Opus:** not built. It needs resampling to 48 kHz, which changes the audio timing.
- **Not tested on real data:** 1x-speed discs, and 18.9 kHz, mono or 8-bit audio.
  None of the three scanned discs have them. Multiple audio tracks are handled in
  the code, but no scanned stream has more than one, and nothing has tested them.
- **Player support:** nothing has been played in a real player. The `h264` preset is
  10-bit with FLAC in MP4, which many players and most hardware decoders don't
  handle, so as a compatibility preset it probably needs 8-bit video and AAC or
  similar. Variable frame rate may cause trouble too.
- **AV1 in MP4:** AV1 has no way to express centre chroma, so only MKV holds that tag.

## Licensing, for distribution

This is an understanding rather than legal advice, so get it checked before a public
release.

- The Gyan FFmpeg builds include x264 and x265, which makes them GPL. Sharing one
  means following the GPL for FFmpeg, including making the corresponding source
  available.
- Linking GPL FFmpeg into `psxvc` would generally make `psxvc` subject to the GPL.
  Running it as a separate program usually doesn't.
- H.264 and H.265 have patent licensing issues in some places.
- For release, build an FFmpeg with only the codecs the presets need. That's also
  when to decide whether x264 and x265 stay, since dropping them could allow an LGPL
  build.
- jPSXdec is a local test tool only and must not ship with the app.
