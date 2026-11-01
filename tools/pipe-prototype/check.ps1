param([string]$Bin, [string]$Index, [int]$Video, [string]$Out, [string]$Proto)
$tags = '-vf','setparams=color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m:range=pc','-color_primaries','smpte170m','-color_trc','smpte170m','-colorspace','smpte170m','-color_range','pc','-chroma_sample_location','2'

function Run($a) { & $Proto $Bin $Index $Video $Out -- @a 2>&1 | ForEach-Object { "  $_" } }
function Times($file, $sel) { @(ffprobe -v error -select_streams $sel -show_entries packet=pts_time -of csv=p=0 $file | ForEach-Object { [double]($_ -replace ',.*','') }) | Sort-Object }

$mkv = "$Out\archive.mkv"; $mp4 = "$Out\h264.mp4"
"== FFV1 + FLAC in MKV"; Run (@('-map','0','-fps_mode','passthrough','-enc_time_base:v','1:150','-c:v','ffv1','-level','3','-g','1','-slicecrc','1','-c:a','flac') + $tags + @($mkv))
"== H.264 + FLAC in MP4"; Run (@('-map','0','-fps_mode','passthrough','-enc_time_base:v','1:150','-video_track_timescale','150','-c:v','libx264','-crf','14','-pix_fmt','yuv420p10le','-c:a','flac') + $tags + @($mp4))

foreach ($f in $mkv,$mp4) {
    "`n== $([IO.Path]::GetFileName($f)): $([math]::Round((Get-Item $f).Length/1MB,1)) MB"
    ffprobe -v error -show_entries stream=codec_name,profile,pix_fmt,color_range,color_space,color_primaries,color_transfer,chroma_location,sample_aspect_ratio,display_aspect_ratio,time_base,sample_rate,channels,avg_frame_rate,r_frame_rate -of compact $f 2>&1 | ForEach-Object { "  $_" }
    $wantV = @(Get-Content "$Out\expected_video_pts.txt" | ForEach-Object { [long]$_ / 150.0 })
    $wantA = @(Get-Content "$Out\expected_audio_pts.txt" | ForEach-Object { [long]$_ / 37800.0 })
    $gotV = Times $f 'v'; $gotA = Times $f 'a'
    "  video packets $($gotV.Count)/$($wantV.Count), first deltas (ms): $((0..5 | ForEach-Object { [math]::Round(($gotV[$_+1]-$gotV[$_])*1000,3) }) -join ' ')"
    $worst = 0.0; for ($i=0; $i -lt $wantV.Count; $i++) { $e = [math]::Abs(($gotV[$i]-$gotV[0]) - ($wantV[$i]-$wantV[0])); if ($e -gt $worst) { $worst = $e } }
    "  worst video timestamp error: $([math]::Round($worst*1000,4)) ms"
    "  A/V offset (audio start minus video start): got $([math]::Round(($gotA[0]-$gotV[0])*1000,3)) ms, expected $([math]::Round(($wantA[0]-$wantV[0])*1000,3)) ms"
    "  audio packets $($gotA.Count) (FLAC repacks, so counts can differ from $($wantA.Count))"
}

"`n== Lossless round trip of $([IO.Path]::GetFileName($mkv))"
ffmpeg -y -hide_banner -loglevel error -i $mkv -map 0:v -f rawvideo -pix_fmt yuv420p10le -fps_mode passthrough "$Out\back.yuv"
ffmpeg -y -hide_banner -loglevel error -i $mkv -map 0:a -f s16le -c:a pcm_s16le "$Out\back.pcm"
"  expected yuv $((Get-Item "$Out\expected.yuv").Length) bytes, got $((Get-Item "$Out\back.yuv").Length)"
"  video identical: $((Get-FileHash "$Out\expected.yuv").Hash -eq (Get-FileHash "$Out\back.yuv").Hash)"
"  expected pcm $((Get-Item "$Out\expected.pcm").Length) bytes, got $((Get-Item "$Out\back.pcm").Length)"
"  audio identical: $((Get-FileHash "$Out\expected.pcm").Hash -eq (Get-FileHash "$Out\back.pcm").Hash)"
