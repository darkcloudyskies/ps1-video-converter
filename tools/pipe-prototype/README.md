# Pipe prototype

Throwaway code that proved the export approach in
[docs/export-notes.md](../../docs/export-notes.md). It's a reference for building
`psx-export` and isn't part of the Cargo workspace or CI. Delete it once
`psx-export` works.

It decodes one video stream and its first audio stream, then pipes them to `ffmpeg`
as a NUT stream. `check.ps1` runs the FFV1 and H.264 exports and checks timestamps,
tags, A/V offset and losslessness. It writes raw decoded video and audio to `-Out`,
so keep that folder out of the repository.

```powershell
cargo build --release --manifest-path tools\pipe-prototype\Cargo.toml --target-dir $env:TEMP\pipe-prototype
psxvc scan "discs\<disc>.cue" -o $env:TEMP\disc.psxidx.json
powershell -NoProfile -ExecutionPolicy Bypass -File tools\pipe-prototype\check.ps1 -Bin (Resolve-Path "discs\<disc>.bin").Path -Index $env:TEMP\disc.psxidx.json -Video <id> -Out $env:TEMP\pipe-out -Proto $env:TEMP\pipe-prototype\release\pipe-proto.exe
```

The stream must have interleaved audio. The timestamp checks assume 150 sectors per
second and 37,800 Hz audio.
