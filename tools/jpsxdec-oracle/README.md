# jPSXdec comparison

Checks `psxvc` against jPSXdec on a real disc. These compare every stream:

- **Video:** per-frame checksums of the MDEC coefficients and the `accurate`
  YCbCr planes. jPSXdec demuxes its own copy of each frame, so this covers our
  demuxer too.
- **Audio:** the decoded PCM, byte for byte.

```powershell
cargo build --release
powershell -NoProfile -ExecutionPolicy Bypass -File tools\jpsxdec-oracle\compare.ps1 -Bin "discs\<disc>.bin"
```

It needs Java and a built jPSXdec jar, by default in a sibling checkout at
`../jpsxdec`. Pass `-Jar` to use another. It writes its working files to
`-WorkDir` (by default under `%TEMP%`). Those hold game data, so keep them out of
the repository.

`FrameHashes.java` calls jPSXdec as a library at run time and doesn't include any
of its code. jPSXdec is under its own non-commercial licence, so don't copy code
from it into this repository.
