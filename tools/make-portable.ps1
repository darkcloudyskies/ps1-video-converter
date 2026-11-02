# Builds the app and puts it in a folder with its own FFmpeg, ready to run or move.
# The app looks for ffmpeg.exe beside itself before trying PATH.
#
# This copies a full FFmpeg build, which is GPL, so the folder is for personal use.
# See "Licensing, for distribution" in docs/export-notes.md before sharing it.
param(
    # The FFmpeg to copy (default: the one on PATH).
    [string]$Ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source,
    [string]$Output = (Join-Path $PSScriptRoot '..\dist\PS1 Video Converter')
)
$ErrorActionPreference = 'Stop'

if (-not $Ffmpeg -or -not (Test-Path $Ffmpeg)) {
    throw 'FFmpeg not found. Put it on PATH or pass -Ffmpeg <path to ffmpeg.exe>.'
}

$root = Join-Path $PSScriptRoot '..'
cargo build --release -p psxvc-gui --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'The build failed.' }

New-Item -ItemType Directory -Force $Output | Out-Null
Copy-Item (Join-Path $root 'target\release\psxvc-gui.exe') (Join-Path $Output 'PS1 Video Converter.exe') -Force
Copy-Item $Ffmpeg (Join-Path $Output 'ffmpeg.exe') -Force
"Ready: $((Resolve-Path $Output).Path)"
