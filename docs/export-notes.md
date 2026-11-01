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
- **Video with the `h264` preset:** always `yuv420p`, with either IDCT. We round to
  8 bits ourselves, so FFmpeg doesn't convert anything.
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
| `h264` | `-c:v libx264 -crf 14 -g 30`, always 8-bit | AAC | MP4 (or MKV with `-o x.mkv`) |
| `av1` | `-c:v libsvtav1 -crf 20 -preset 6 -g 30` | FLAC | MKV (or MP4 with `-o x.mp4`) |

MP4 output also needs `-video_track_timescale <sectors per second>`.

`--audio flac|opus|aac` overrides the preset's audio, except on `archive`, which only
allows FLAC because it's lossless. Audio options are left out for a stream with no
audio, since FFmpeg warns about unused ones.

| Audio | Arguments |
|---|---|
| FLAC | `-c:a flac`, at the native sample rate |
| Opus | `-c:a libopus -b:a 128k -ar 48000` |
| AAC | `-c:a aac -b:a 192k -ar 48000` |

Opus and AAC have no 37.8 kHz mode, so they resample. The bitrates are starting
points that nobody has judged by listening.

### Constant frame rate

`--cfr` adds an `fps` filter before `setparams` and keeps `-fps_mode passthrough`, so
there's one place where frames get repeated or dropped:

```text
-vf fps=fps=<n>/<d>:start_time=0:round=near,setparams=...
-enc_time_base:v <d>:<n>    (MP4: -video_track_timescale <n>)
```

- **Rate:** whichever of two candidates fits the frames' sectors best: a whole number
  of sectors per frame (15 fps), or that slowed by 1000/1001 for NTSC (15000/1001). A
  candidate fits if the frames furthest ahead of and behind its grid are at most 4
  sectors apart. If neither does, the stream gets its exact average.
- **`start_time=0`:** audio often starts before the first frame. The filter repeats
  the first frame until then, so the picture stays in sync.
- **The last frame** may be held for one extra tick, because its length isn't known.
- **Dropped frames:** if two frames land on the same tick, the export warns.

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
| Opus in MKV and MP4: video timestamps as above, audio lag under 1 ms | Shortest with audio on each disc | Pass |
| `--cfr` in FFV1: the right picture on every tick, FLAC unchanged | All 67 | Pass |
| `--cfr` in MKV and MP4: evenly spaced timestamps, rate tagged in MP4 | All 67 | Pass |
| AAC in MP4: audio lag under 1 ms | All 52 streams with both video and audio | Pass |

Audio lag came out at 0 ms in all 65 measurements. It's measured by finding the shift
that best lines up the decoded audio with the original over its loudest stretch. The test also shifts the original by one sector
to confirm that the measurement sees the change.

With `--cfr`, 46 streams came out at 15 fps, 19 at 15000/1001, and one at 30 fps. One
Parasite Eve Disc 1 stream of 2,077 frames fell back to its exact average, 311400/20783
(14.983 fps). No stream dropped a frame.

Exporting the 36 streams of Parasite Eve II Disc 1 (19,183 frames, down to 48x32)
took 12 s and 0.72 GB with `archive`, 17 s and 154 MB with `h264`, and 24 s and
121 MB with `av1`, with no failures. The only warnings are from SVT-AV1 on the two
48x32 streams, where it turns off some of its tools because the frame is so small.
That's fast enough that decoding doesn't need `rayon` for now. These timings predate
the `h264` preset's move to 8-bit and AAC.

### Player testing

Tried in Chromium 152, the browser built into the Claude desktop app, with
one 5.5 s Parasite Eve II stream (320x192). Each file was given 2.5 s to play, with no
dropped frames, and the audio decoded, though nobody listened to it. All reached about
2.45 s except H.264 10-bit, which reached 1.39 s. It was the first file loaded in its
run, so that may be start-up time rather than decoding, and this doesn't separate them.

| File (all MP4) | Result |
|---|---|
| H.264 8-bit and AAC, with and without `--cfr` | Plays |
| AV1 10-bit and Opus | Plays |
| H.264 8-bit, H.264 10-bit, AV1 10-bit, each with FLAC | Plays |

On the FLAC files, a frame at 2.70 s was also compared with `psxvc frame`. The mean
difference was under 1.5 of 255 per channel, with no overall shift, so Chromium is
reading full range and BT.601 correctly and shows the right frame for that time.

Chromium decodes these in software, so it doesn't show how hardware decoders or TVs
would behave.

The owner also tried a 2-minute Parasite Eve II stream (320x240, variable frame rate)
in the Windows default player, thought to be the Photos app:

| File | Result |
|---|---|
| `h264` in MP4 | Plays well |
| `av1` with Opus in MKV | Plays, but seeking back and forth misbehaved |
| `archive` in MKV | Black picture |
| 12-bar luma test, tagged like `h264` | All 12 bars distinct, so the full-range tag is honoured |

- **Black `archive`:** FFmpeg decodes the same file to a normal picture, so the file
  is fine. The player probably has no FFV1 decoder, though that isn't confirmed.
  `archive` is for preservation, so this is accepted for now.
- **AV1 seeking:** that file had key frames up to 10.74 s apart, which is the likely
  cause. Both lossy presets now use `-g 30`, which brings the longest gap to about 2 s
  with little change in size. The new file hasn't been retried yet.

## Why `h264` is 8-bit with AAC

Chromium plays 10-bit H.264 with FLAC, so this wasn't forced by a failed test. The
preset exists for compatibility, and as far as I know most hardware H.264 decoders
don't handle 10-bit, and many players don't handle FLAC in MP4. Nothing here has
tested that. Anyone who wants 10-bit or lossless audio has `av1` and `archive`.

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

- **Not tested on real data:** 1x-speed discs, and 18.9 kHz, mono or 8-bit audio.
  None of the three scanned discs have them. Multiple audio tracks are handled in
  the code, but no scanned stream has more than one, and nothing has tested them.
- **AV1 seeking:** retry it in the Windows default player now that key frames are
  closer together.
- **Player support:** only Chromium and the Windows default player have been tried
  (see Player testing). Still to try, if they matter: VLC or mpv, and a TV, phone or
  other hardware decoder, which may struggle with full-range H.264 or variable frame
  rate. A device that ignores the full-range tag treats 16 as black and 235 as white, so
  shadows and highlights get crushed and the picture looks too contrasty, not washed
  out. To check, play a clip of bars with luma 0, 6, 12, 18, 24, 30 and 225, 231, 237,
  243, 249, 255, tagged like the `h264` preset: if all 12 are distinct, the tag is
  honoured. If not, the fix is converting to limited range for that preset.
- **Listening:** nobody has judged the Opus and AAC bitrates by ear.
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
