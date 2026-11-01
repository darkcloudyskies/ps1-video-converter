# Compares psxvc with jPSXdec on every video and audio stream of a disc.
# See README.md beside this file. Everything it writes goes under -WorkDir.
param(
    [Parameter(Mandatory)][string]$Bin,
    [string]$Jar,
    [string]$WorkDir = "$env:TEMP\psxvc-oracle",
    [string]$Psxvc
)
# Windows PowerShell leaves $PSScriptRoot empty in parameter defaults.
if (-not $Jar) { $Jar = "$PSScriptRoot\..\..\..\jpsxdec\jpsxdec\_ant\release\jpsxdec.jar" }
if (-not $Psxvc) { $Psxvc = "$PSScriptRoot\..\..\target\release\psxvc.exe" }
$Bin = (Resolve-Path -LiteralPath $Bin).Path
$Jar = (Resolve-Path -LiteralPath $Jar).Path
$Psxvc = (Resolve-Path -LiteralPath $Psxvc).Path
$name = [IO.Path]::GetFileNameWithoutExtension($Bin) -replace '[^A-Za-z0-9]', '_'
$work = Join-Path $WorkDir $name
New-Item -ItemType Directory -Force $work | Out-Null

$classes = Join-Path $work 'classes'
& javac -nowarn -cp $Jar -d $classes "$PSScriptRoot\FrameHashes.java"
if ($LASTEXITCODE -ne 0) { throw 'javac failed' }

$idx = Join-Path $work 'disc.idx'
$pidx = Join-Path $work 'disc.psxidx.json'
Push-Location $work
try {
    if (-not (Test-Path $idx)) { & java -jar $Jar -f $Bin -x $idx | Out-Null }
    & $Psxvc scan --quiet $Bin -o $pidx | Out-Null
    $index = [IO.File]::ReadAllText($pidx) | ConvertFrom-Json

    # jPSXdec's item number and sector range, for each video and audio item.
    $items = foreach ($line in [IO.File]::ReadAllLines($idx)) {
        if ($line -match '^#:(\d+)\|.*?\|Sectors:(\d+)-(\d+)\|Type:(Video|XA)\b') {
            [pscustomobject]@{ Num = $Matches[1]; First = [int]$Matches[2]; Last = [int]$Matches[3]; Type = $Matches[4] }
        }
    }

    $badVideo = 0; $frames = 0; $badFrames = 0; $noMatch = 0
    foreach ($v in $index.video) {
        $item = $items | Where-Object { $_.Type -eq 'Video' -and $_.First -eq $v.first_lba } | Select-Object -First 1
        if (-not $item) { "video $($v.id): no jPSXdec item at sector $($v.first_lba)"; $noMatch++; continue }
        $dir = Join-Path $work "v$($v.id)"
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $dir
        New-Item -ItemType Directory $dir | Out-Null
        & java -jar $Jar -x $idx -i $item.Num -vf bs -num Header -noaud -dir $dir 2>&1 | Out-Null
        $bsDir = Get-ChildItem -LiteralPath $dir -Recurse -Filter *.bs | Select-Object -First 1 | ForEach-Object { $_.DirectoryName }
        $want = @(& java -cp "$classes;$Jar" FrameHashes $bsDir $v.width $v.height)
        $got = @(& $Psxvc frame-hashes $Bin --stream $v.id --index $pidx)
        $diff = 0
        for ($i = 0; $i -lt [Math]::Max($want.Count, $got.Count); $i++) {
            if ($want[$i] -cne $got[$i]) {
                if ($diff -eq 0) { "  first difference, line ${i}:`n    jPSXdec $($want[$i])`n    psxvc   $($got[$i])" }
                $diff++
            }
        }
        $frames += $got.Count; $badFrames += $diff
        if ($diff) { $badVideo++ }
        "video $($v.id) $($v.file) $($v.width)x$($v.height) $($v.variant): $($got.Count) frames, $diff differ"
        Remove-Item -Recurse -Force $dir
    }

    $badAudio = 0
    foreach ($a in $index.audio) {
        $item = $items | Where-Object { $_.Type -eq 'XA' -and $_.First -eq $a.first_lba } | Select-Object -First 1
        if (-not $item) { "audio $($a.id): no jPSXdec item at sector $($a.first_lba)"; $noMatch++; continue }
        $dir = Join-Path $work "a$($a.id)"
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $dir
        New-Item -ItemType Directory $dir | Out-Null
        & java -jar $Jar -x $idx -i $item.Num -dir $dir 2>&1 | Out-Null
        $theirs = Get-ChildItem -LiteralPath $dir -Recurse -Filter *.wav | Select-Object -First 1
        $ours = Join-Path $dir 'psxvc-out.bin'
        & $Psxvc audio $Bin --stream $a.id --index $pidx -o $ours | Out-Null
        $x = [IO.File]::ReadAllBytes($theirs.FullName)
        $y = [IO.File]::ReadAllBytes($ours)
        # Compare the PCM only; each writes its own header. Ours is 44 bytes.
        $pcm = $y.Length - 44
        $same = $x.Length -ge $pcm
        $start = $x.Length - $pcm
        if ($same) {
            $same = [Linq.Enumerable]::SequenceEqual([byte[]]($x[$start..($x.Length - 1)]), [byte[]]($y[44..($y.Length - 1)]))
        }
        if (-not $same) { $badAudio++ }
        "audio $($a.id) $($a.file): $pcm bytes of PCM, $(if ($same) { 'identical' } else { 'DIFFERENT' })"
        Remove-Item -Recurse -Force $dir
    }

    ''
    "SUMMARY $name"
    "  video: $($index.video.Count) streams, $frames frames; $badVideo streams and $badFrames frames differ"
    "  audio: $($index.audio.Count) streams; $badAudio differ"
    "  streams with no matching jPSXdec item: $noMatch"
}
finally {
    Pop-Location
}
