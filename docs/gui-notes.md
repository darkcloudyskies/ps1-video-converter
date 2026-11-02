# GUI notes

The desktop app is `psxvc-gui`. To run it:

```text
cargo run --release -p psxvc-gui
```

It needs only Rust and WebView2, which Windows 11 includes. FFmpeg is found the same
way as for the CLI: beside the program, then on PATH.

To get a folder that holds the app and its own FFmpeg, about 230 MB, and can be moved
anywhere on a machine with WebView2:

```text
tools\make-portable.ps1
```

It ends up in `dist\PS1 Video Converter`, which git ignores. An export from that folder
with FFmpeg off PATH came out byte-identical to the CLI's.

## How it's laid out

- **`psxvc-app`** does the work and has no UI toolkit in it. Its tests run anywhere,
  while Tauri would need WebKit on Linux CI, and Tauri test programs can fail to start
  on Windows.
- **`psxvc-gui`** only connects the page to `psxvc-app`.
- **`psxvc-gui/ui/`** is plain HTML, CSS and JS, so there's no Node or build step.
  Tauri puts it inside the program when it compiles.

## Decisions

- **Frames travel as raw RGBA, not PNG:** there's no encoding step, and decoding took
  about 2.4 ms per frame (median of 40, 256x176, release build). While dragging, only
  the newest wanted frame is decoded next, so no backlog builds up.
- **The preview is 8-bit RGB:** that's how a screen shows it, so `hq`'s 10-bit precision
  can't be seen in the viewer. Exports keep it, except with `h264`.
- **Comparison has a difference pane**, brightened 16 times. The plan only asked for side
  by side, but the two IDCTs differ by at most 2 or 3 of 255 on the frames tried, which
  nobody can see unaided.
- **One export at a time:** FFmpeg's encoders already use the available cores, and the
  order stays predictable. A failed export doesn't stop the rest.
- **Exports never replace a file:** a name that's taken gets `-2`, `-3` and so on. The
  CLI still replaces, since there the output is named explicitly or the user is at the
  keyboard. The name is chosen as the job starts, so it sees what earlier jobs wrote.
- **Cancelling** makes the disc reader start failing, and `export_stream` already handles
  a failed read by closing FFmpeg's pipe and deleting the partial file. Closing the
  window does the same to any running export.
- **The saved index** is `<name>.psxidx.json` beside the disc, shared with the CLI. The
  app rescans if it's unreadable, from another format version, or from a disc of another
  size. It can't tell two discs of exactly the same size apart.
- **`h264` is the first preset in the list**, since it's the format the owner mainly
  uses. The CLI's default is still `archive`.

## What's been checked

In the release build, driving the real window through WebView2's debugging port, on
Parasite Eve Disc 1, Parasite Eve II Disc 1 and Koudelka Disc 4:

| Check | Result |
|---|---|
| Each disc opens, lists its streams and shows a frame | Pass (21, 36 and 10 streams) |
| Scrubber, arrow key, previous button, the three views | Pass |
| `h264` export is byte-identical to the CLI's | Pass, one 70-frame stream |
| `archive` and `av1` exports: packets match the CLI's, and `archive`'s timestamps | Pass, same stream |
| Ticking two streams and queueing them | Pass |
| A second export of the same stream gets `-2` | Pass |
| Cancelling a running export: no partial file, and the next job runs | Pass |
| Closing the window mid-export: no partial file, no FFmpeg left running | Pass |
| No FFmpeg: the job fails and says why | Pass |
| Opening a disc again uses the saved index | Pass, 13 ms |

MKV files can't be compared byte for byte, because two CLI runs differ from each other too.

## Not checked

- **The file and folder dialogs by hand:** the page's permission to open one was
  accepted, but nobody has picked a file through them. The tests call the same code
  directly with a path.
- **Whether FFmpeg's console window stays hidden:** the flag is set, and it's only visible
  to someone watching the screen during an export.
- **A disc with damaged sectors or a broken frame in the viewer:** none of the three has
  any. The warnings are written but have never been shown.
- **Light mode:** the machine used is in dark mode.

## If it's ever released publicly

The app is for the owner's own use, so the portable folder simply copies their full,
GPL FFmpeg build. Sharing that folder would mean following the GPL for FFmpeg. A release
would need:

- **A trimmed FFmpeg:** a build with only the presets' codecs, as a Tauri sidecar. Keeping
  `h264` means keeping x264, which makes that build GPL. See "Licensing, for distribution"
  in [export-notes.md](export-notes.md).
- **An installer:** `bundle.active` is off in `tauri.conf.json`, and building one needs
  `cargo-tauri`, which isn't installed.
