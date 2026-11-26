[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [Parameter(Mandatory = $true)][string]$ProtocExe,
    [Parameter(Mandatory = $true)][string]$PythonExe,
    [Parameter(Mandatory = $true)][string]$UvExe,
    [switch]$Generate,
    [string]$CocoReaderPythonExe,
    [string]$PackagedWorkerExe,
    [string]$VideoFfmpegExe,
    [string]$VideoFfprobeExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$protoRoot = Join-Path $repo 'contracts\proto'
$worker = Join-Path $repo 'services\python-worker'
$dev = Join-Path $repo 'target\dev'
$run = Join-Path $repo "target\contract-check\worker-rpc\$([guid]::NewGuid().ToString('N'))"
$staged = Join-Path $run 'generated'
$tracked = Join-Path $worker 'src\dd_worker\generated'
New-Item -ItemType Directory -Path $staged -Force | Out-Null

$env:UV_PROJECT_ENVIRONMENT = Join-Path $dev 'worker-venv'
$cache = Join-Path $dev 'uv-cache'
& $UvExe sync --locked --offline --no-build --no-python-downloads --no-config --project $worker --cache-dir $cache --python $PythonExe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:DD_WORKER_PYTHON = Join-Path $env:UV_PROJECT_ENVIRONMENT 'Scripts\python.exe'
$env:DD_CORE_CLAIM_RUN_DIR = $run
if ($PackagedWorkerExe) {
    $env:DD_PACKAGED_WORKER_EXE = (Resolve-Path -LiteralPath $PackagedWorkerExe).Path
} else {
    Remove-Item Env:DD_PACKAGED_WORKER_EXE -ErrorAction SilentlyContinue
}
if ([bool]$VideoFfmpegExe -ne [bool]$VideoFfprobeExe) { throw 'Pass both video tool executables.' }
if ($VideoFfmpegExe) {
    $env:DD_WORKER_VIDEO_FFMPEG = (Resolve-Path -LiteralPath $VideoFfmpegExe).Path
    $env:DD_WORKER_VIDEO_FFPROBE = (Resolve-Path -LiteralPath $VideoFfprobeExe).Path
} else {
    Remove-Item Env:DD_WORKER_VIDEO_FFMPEG, Env:DD_WORKER_VIDEO_FFPROBE -ErrorAction SilentlyContinue
}
$generateArgs = @('-I', '-B', '-m', 'grpc_tools.protoc', "--proto_path=$protoRoot",
    "--python_out=$staged", "--pyi_out=$staged")
& $env:DD_WORKER_PYTHON @generateArgs (Join-Path $protoRoot 'datasetdoctor\v1\common.proto')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $env:DD_WORKER_PYTHON @generateArgs "--grpc_python_out=$staged" (Join-Path $protoRoot 'datasetdoctor\v1\worker_service.proto')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $env:DD_WORKER_PYTHON @generateArgs "--grpc_python_out=$staged" (Join-Path $protoRoot 'datasetdoctor\v1\core_execution.proto')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$stagedFiles = @(Get-ChildItem -LiteralPath $staged -Recurse -File)
if ($Generate) {
    foreach ($file in $stagedFiles) {
        $relative = $file.FullName.Substring($staged.Length + 1)
        $target = Join-Path $tracked $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $target
    }
}
$trackedFiles = @(Get-ChildItem -LiteralPath $tracked -Recurse -File | Where-Object Extension -In '.py','.pyi')
if ($stagedFiles.Count -ne $trackedFiles.Count) { throw 'Generated Python stub file count changed.' }
foreach ($file in $stagedFiles) {
    $relative = $file.FullName.Substring($staged.Length + 1)
    $target = Join-Path $tracked $relative
    if ((Get-FileHash -LiteralPath $file.FullName).Hash -cne (Get-FileHash -LiteralPath $target).Hash) {
        throw "Python Protobuf stub drift: $relative"
    }
}

$env:CARGO_HOME = (Resolve-Path -LiteralPath (Join-Path $dev 'cargo-home')).Path
$env:CARGO_TARGET_DIR = Join-Path $dev 'cargo-target'
$env:RUSTC = Join-Path $RustBin 'rustc.exe'
$env:PROTOC = (Resolve-Path -LiteralPath $ProtocExe).Path
$cargo = Join-Path $RustBin 'cargo.exe'
$manifest = Join-Path $repo 'Cargo.toml'
Write-Output "Python stub drift check passed. Worker RPC evidence: $run"
$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false
& $cargo test --manifest-path $manifest --locked --offline --target x86_64-pc-windows-msvc -p dd-protocol --test worker_loopback -- --nocapture 2>&1 |
    ForEach-Object { $_.ToString() } | Tee-Object -FilePath (Join-Path $run 'check.log')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$tests = @('--test', 'core_claim_loopback', '--test', 'worker_geometry', '--test', 'worker_process',
    '--test', 'worker_color_preview', '--test', 'workspace_geometry', '--test', 'review_draft', '--test', 'review_final',
    '--test', 'workspace_backup_task')
if ($PackagedWorkerExe) {
    $tests = @('--test', 'worker_color_preview')
}
if ($VideoFfmpegExe) {
    $tests += @('--test', 'worker_video_probe', '--test', 'worker_frame_resolve')
    if (-not $PackagedWorkerExe) {
        $tests += @('--test', 'workspace_video_probe')
    }
}
if ($CocoReaderPythonExe) {
    $env:DD_COCO_READER_PYTHON = (Resolve-Path -LiteralPath $CocoReaderPythonExe).Path
    $tests += @('--test', 'coco_export')
}
& $cargo test --manifest-path $manifest --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --features execution-rpc @tests -- --nocapture 2>&1 |
    ForEach-Object { $_.ToString() } | Tee-Object -FilePath (Join-Path $run 'check.log') -Append
exit $LASTEXITCODE
