# Export notes for Phase 3

Phase 3 builds `psx-export` by piping decoded frames and audio to an `ffmpeg`
process. A prototype proved this on real discs, and it's in
[tools/pipe-prototype](../tools/pipe-prototype). It's a reference to rebuild from,
not code to extend. Delete it once `psx-export` works.

## Why a pipe instead of `rsmpeg`

The final GUI app has to include everything it needs, and that favours shipping
`ffmpeg.exe` beside the app (a Tauri sidecar) over linking FFmpeg:

- **Build:** it needs only `cargo build`, with no clang or FFmpeg development
  files on any build machine, including CI.
- **Versions:** when checked (November 2026), the latest `rsmpeg` was
  `0.18.0+ffmpeg.8.0`, a major version behind the installed FFmpeg 9.0.2. The
  bindings were never test-built against FFmpeg 9.
- **Licensing:** see below.

Keep FFmpeg behind a small interface in `psx-export`, so the rest of the code
doesn't depend on how it's run.

## How the pipe works

`psxvc` writes one NUT stream to `ffmpeg`'s stdin, holding both video and audio.
Using one pipe avoids juggling two on Windows, and NUT stores exact rational
timestamps per stream, which raw video pipes and Y4M can't.

- **Video:** `yuv420p10le`, tag `Y3 0B 0A` (bytes `59 33 0B 0A`), time base
  1/(sectors per second). A frame's PTS is its `last_lba` minus the base sector.
- **Audio:** `pcm_s16le`, tag `PSD 0x10`, time base 1/(sample rate), one packet per
  sector. Its PTS is the sector's offset from the base sector, in samples:
  `(lba - base) * rate / sectors_per_second`.
- **Base sector:** the lower of the video's and audio's first sectors, so both use
  the same clock.
- **Order:** disc order, which is already correctly interleaved.
- **10-bit samples:** full range, so luma is `(y + 128) * 1023 / 255` and chroma is
  `c * 1023 / 255 + 512`, rounded and clamped to 0..1023.

### NUT details that matter

The prototype's `Nut` writer shows all of this in about 100 lines.

- FFmpeg rejects frames with `header_idx invalid` unless the main header has the
  field that's optional in the spec, the elision header count. Write 0 there.
- Each frame gets a syncpoint before it. Each frame has `FLAG_CHECKSUM` set, because
  frames larger than `2 * max_distance` must have one.
- A packet's checksum covers the data after `forward_ptr`, using CRC-32 with
  polynomial `0x04C11DB7`, initial value 0, written big-endian.
- `ffmpeg` prints `Guessed Channel Layout: stereo`, which is harmless.

## FFmpeg arguments

```text
ffmpeg -y -color_range pc -f nut -i pipe:0 -map 0
  -fps_mode passthrough -enc_time_base:v 1:150
  -vf setparams=color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m:range=pc
  -color_primaries smpte170m -color_trc smpte170m -colorspace smpte170m
  -color_range pc -chroma_sample_location 2
  <codec arguments> <output>
```

The tests only used 150 sectors per second, so `1:150` should follow the stream's
disc speed.

- **Archive:** `-c:v ffv1 -level 3 -g 1 -slicecrc 1 -c:a flac`, in MKV.
- **H.264:** `-video_track_timescale 150 -c:v libx264 -crf 14 -pix_fmt yuv420p10le
  -c:a flac`, in MP4.

Three of these are easy to get wrong:

- **`-enc_time_base:v`:** without it, FFmpeg rounds PTS to the encoder's guessed
  frame rate and silently produces constant frame rate. The result still passes the
  lossless check, so tests must compare timestamps too.
- **`setparams`:** without it, primaries and transfer come out `unknown`.
- **`-color_range pc` before `-i`:** this applies to the input and is separate from
  the one on the output.

## Prototype results

Tested on Parasite Eve Disc 1 `/FMV1/FMV016B.STR` (STRv2, 97 frames) and Parasite
Eve II Disc 1 `/INTER0.STR` stream 15 (STRv3, 320x192, 737 frames). Frames there are
9, 10 or 11 sectors apart, so these are true variable frame rate streams.

| Check | FFV1 + FLAC in MKV | H.264 + FLAC in MP4 |
|---|---|---|
| Worst video timestamp error | 0.67 ms | 0.0007 ms (`ffprobe`'s printed precision) |
| A/V offset (expected -13.333 ms) | -13 ms | -13.333 ms |
| Decodes back byte-identical | Video and audio | Not applicable |
| `yuv420p10le`, full range, BT.601, centre chroma | Yes | Yes |
| Audio at 37,800 Hz | Yes | Yes |

MKV's rounding comes from FFmpeg's MKV writer, which uses a 1 ms time base. It
doesn't build up over a stream. Linking FFmpeg wouldn't change it.

## Still to do or decide

- **Not tested:** AV1, Opus, 1x-speed discs, 18.9 kHz or mono audio, and whole-disc
  runs. Opus needs resampling to 48 kHz, which changes the audio timing.
- **Player support:** nothing has been played in a real player. Variable frame rate
  and FLAC in MP4 can be poorly supported, so the H.264 preset may need another
  audio codec.
- **Aspect ratio:** the prototype marks all pixels square, so 320x192 comes out as
  5:3 rather than 4:3. The right value depends on how each game displayed these
  streams, so this needs a rule.
- **Exact timestamps in the archive:** MKV can't hold them through FFmpeg's writer.
  Decide whether 1 ms is acceptable for the archive preset, or whether it needs
  another route.
- **Stream lifecycle:** the prototype decodes all audio up front. Real export should
  stream, report FFmpeg failures properly and avoid deadlocks on its output.

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
