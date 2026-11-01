# Target games

The first acceptance targets: **Parasite Eve** (2 discs), **Parasite Eve II**
(2 discs) and **Koudelka** (4 discs). For now, only the 3 discs scanned below matter.
The others can be filled in if they're picked up later.

The results below come from `psxvc scan`, compared with jPSXdec v2.1 (beta). To
fill in the rest, run on each disc:

```bash
psxvc scan "<disc>.cue"
```

and, for comparison, jPSXdec's indexer. It needs the `.bin` and rejects a `.cue`:

```bash
java -jar jpsxdec.jar -f "<disc>.bin" -x "<disc>.idx"
```

Add the `psxvc scan` summary and any differences from the `.idx` file here (they
hold stream descriptions only, no game data).

| Game | Disc | Video streams | Variant(s) | Resolution | fps | Audio |
|---|---|---|---|---|---|---|
| Parasite Eve | 1 | 21 | STRv2 | 320x240 | 14.985 (15 for `FMV016B.STR`) | XA 37.8 kHz stereo 4-bit (21 streams) |
| Parasite Eve | 2 | not scanned | | | | |
| Parasite Eve II | 1 | 36 | STRv2 (25), STRv3 (11) | 320x240, 320x192, smaller sizes down to 48x32 | 15; 30 for `/DUMMY.DMY` | XA 37.8 kHz stereo 4-bit (21 streams) |
| Parasite Eve II | 2 | not scanned | | | | |
| Koudelka | 1–3 | not scanned | | | | |
| Koudelka | 4 | 10 | STRv2, plus unsupported `/MOVIE/ENDING.MCF` | 320x240 | 15 | XA 37.8 kHz stereo 4-bit (11 streams) |

All three scanned discs reported no EDC errors and no incomplete frames.

Parasite Eve runs at 10.01 sectors per frame (jPSXdec reports 1001/100), which is
where 14.985 fps comes from. Parasite Eve II streams with no interleaved audio
(`/STAGE1.CDF`, `/STAGE2.CDF` and `/DUMMY.DMY`) have no disc speed in either tool,
so their fps assumes 2x.

## Comparison with jPSXdec

- **Video:** all 67 streams match on sector range, dimensions and frame count.
- **Variants:** the `.idx` file doesn't record them, but decoding confirms them:
  jPSXdec accepts every frame as the variant we report.
- **Audio:** all 53 streams match on sector range.

### Decoding

`tools/jpsxdec-oracle/compare.ps1` compares decoded output on every stream:

| Disc | Video streams | Frames | Frames that differ | Audio streams | Audio that differs |
|---|---|---|---|---|---|
| Koudelka Disc 4 | 10 | 9,596 | 0 | 11 | 0 |
| Parasite Eve Disc 1 | 21 | 10,497 | 0 | 21 | 0 |
| Parasite Eve II Disc 1 | 36 | 19,183 | 0 | 21 | 0 |

For video, this compares the MDEC coefficients and the `accurate` YCbCr planes of
each frame by checksum, using jPSXdec's own demux of the frame. For audio, it
compares the PCM byte for byte.

This doesn't cover:

- **`hq` output:** it's different from jPSXdec by design, so unit tests check it
  against the textbook IDCT formula instead.
- **XA mono, 18.9 kHz or 8-bit audio:** none of these discs use them, so only unit
  tests cover them.
- **`/MOVIE/ENDING.MCF` on Koudelka Disc 4:** see the open issue below.

## Left out

- **Koudelka Disc 4, `/MOVIE/ENDING.MCF`:** the owner doesn't need this file, so
  nobody is working on it. What's known so far: jPSXdec also finds only the audio, at
  1x disc speed, in a repeating pattern of 1 audio sector then 3 data sectors. All
  17,949 data sectors start with `60 01 01 00`, where STR uses `60 01 01 80`, and
  have no width or height at the STR offsets. Bytes 8–11 count up once per sector.
  This is a custom format that neither tool supports. It's probably the 5:19 ending
  movie, but nothing has been decoded to confirm that.

## Settled questions

- **Parasite Eve II Disc 1, `/INTER0.STR`:** each video has its own audio stream.
  On this file, XA audio runs without a break across some video boundaries, with
  no end-of-file flag, so the indexer also splits audio where a new video starts.
- **Parasite Eve II Disc 1, `/DUMMY.DMY`:** this is real video, not a false
  detection. Sampled frames decode cleanly in jPSXdec to coherent CG footage.
- **Parasite Eve Disc 1, `/FMV1/FMV000.STR`:** the short audio (0:02 of a 0:10
  video) is reported by both tools, so it isn't a `psxvc` error.
