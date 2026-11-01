# Target games

The first acceptance targets: **Parasite Eve** (2 discs), **Parasite Eve II**
(2 discs) and **Koudelka** (4 discs).

The results below come from `psxvc scan` alone, so they aren't confirmed until
they've been compared with jPSXdec. To fill in the rest, run on each disc:

```bash
psxvc scan "<disc>.cue"
```

and, for comparison, jPSXdec's indexer:

```bash
java -jar jpsxdec.jar -f "<disc>.cue" -x "<disc>.idx"
```

Add the `psxvc scan` summary and the `.idx` file here (they hold stream
descriptions only, no game data).

| Game | Disc | Video streams | Variant(s) | Resolution | fps | Audio |
|---|---|---|---|---|---|---|
| Parasite Eve | 1 | 21 | STRv2 | 320x240 | about 15 | XA 37.8 kHz stereo 4-bit (21 streams) |
| Parasite Eve | 2 | not scanned | | | | |
| Parasite Eve II | 1 | 36 | STRv2 (25), STRv3 (11) | 320x240, 320x192, smaller sizes down to 48x32 | 15; 30 for `/DUMMY.DMY` | XA 37.8 kHz stereo 4-bit (6 streams) |
| Parasite Eve II | 2 | not scanned | | | | |
| Koudelka | 1–3 | not scanned | | | | |
| Koudelka | 4 | 10 | STRv2 | 320x240 | about 15 | XA 37.8 kHz stereo 4-bit (11 streams) |

All three scanned discs reported no EDC errors.

Parasite Eve II streams with no interleaved audio (`/STAGE1.CDF`, `/STAGE2.CDF`
and `/DUMMY.DMY`) get their fps from an assumed 2x disc speed.

## Open questions

These are readings of the scan output. None has been checked against the sector
data or jPSXdec yet.

- **Koudelka Disc 4, `/MOVIE/ENDING.MCF`:** 5:19 of audio with no video detected.
  The audio stride of 4 fits a 1x disc speed, which leaves most sectors for
  something else, possibly video in a custom format. If so, that format moves
  into phase 1.
- **Parasite Eve II Disc 1, `/DUMMY.DMY`:** detected as 4,948 frames of 256x176
  at 30 fps. It may be leftover padding, not a real video.
- **Parasite Eve II Disc 1, `/INTER0.STR`:** one audio stream spans video streams
  15–26, so export has to cut the audio to match each video, unless they turn
  out to be one sequence.
- **Parasite Eve Disc 1, `/FMV1/FMV000.STR`:** 0:10 of video but only 0:02 of
  audio.
