[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $FfmpegExe,
    [Parameter(Mandatory)][string] $FfprobeExe,
    [Parameter(Mandatory)][string] $PythonExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$env:DD_TEST_FFMPEG = (Resolve-Path -LiteralPath $FfmpegExe).Path
$env:DD_TEST_FFPROBE = (Resolve-Path -LiteralPath $FfprobeExe).Path
$python = (Resolve-Path -LiteralPath $PythonExe).Path
$run = New-Item -ItemType Directory -Force (Join-Path $repo ('target/p08/video-decoder/' + [guid]::NewGuid().ToString('N')))
$env:DD_TEST_VIDEO_RUN = $run.FullName
$log = Join-Path $run.FullName 'check.log'
$fixtures = Join-Path $repo 'tests/worker/video'
Write-Host "Windows video decoder evidence: $log"

$ErrorActionPreference = 'Continue'
& $python -I -B -m unittest discover -s $fixtures -p 'test_video_*.py' -v *> $log
$code = $LASTEXITCODE
Get-Content -LiteralPath $log
Add-Content -LiteralPath $log -Value "exit_code=$code"
exit $code
