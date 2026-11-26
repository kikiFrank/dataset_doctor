[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [string]$CocoReaderPythonExe,
    [string]$VideoFfmpegExe,
    [string]$VideoFfprobeExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
if ($env:VSCMD_ARG_TGT_ARCH -ne 'x64') { throw 'Initialize x64 VS Developer PowerShell.' }
$dev = Join-Path $repo 'target\dev'
$cargo = (Resolve-Path -LiteralPath (Join-Path $RustBin 'cargo.exe')).Path
$env:RUSTC = (Resolve-Path -LiteralPath (Join-Path $RustBin 'rustc.exe')).Path
$env:CARGO_HOME = (Resolve-Path -LiteralPath (Join-Path $dev 'cargo-home')).Path
$env:CARGO_TARGET_DIR = Join-Path $dev 'cargo-target'
$run = Join-Path $repo "target\desktop-commands\runs\$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $run -Force | Out-Null
$env:DD_DESKTOP_COMMANDS_RUN_DIR = $run
if ([bool]$VideoFfmpegExe -ne [bool]$VideoFfprobeExe) { throw 'Pass both video tool executables.' }
if ($VideoFfmpegExe) {
    $env:DD_WORKER_VIDEO_FFMPEG = (Resolve-Path -LiteralPath $VideoFfmpegExe).Path
    $env:DD_WORKER_VIDEO_FFPROBE = (Resolve-Path -LiteralPath $VideoFfprobeExe).Path
} else {
    Remove-Item Env:DD_WORKER_VIDEO_FFMPEG, Env:DD_WORKER_VIDEO_FFPROBE -ErrorAction SilentlyContinue
}
$log = Join-Path $run 'check.log'
$args = @('test', '--manifest-path', (Join-Path $repo 'Cargo.toml'), '--locked', '--offline',
    '--target', 'x86_64-pc-windows-msvc', '-p', 'dd-desktop', 'workspace_commands::')
if ($VideoFfmpegExe) { $args += @('--features', 'video-acceptance', '--', '--nocapture') }
"Running desktop host tests without starting WebView2. Log: $log" | Tee-Object -FilePath $log
$ErrorActionPreference = 'Continue'
& $cargo @args 2>&1 | ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
$result = $LASTEXITCODE
if ($result -eq 0 -and $CocoReaderPythonExe) {
    $reader = (Resolve-Path -LiteralPath $CocoReaderPythonExe).Path
    $package = Get-Content -LiteralPath (Join-Path $run 'export-reader-package.txt') -Raw
    & $reader -I -B (Join-Path $repo 'tests/integration/rpc/read_coco_export.py') $package --desktop-negative 2>&1 |
        ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
    $result = $LASTEXITCODE
}
"exit_code=$result" | Tee-Object -FilePath $log -Append
exit $result
