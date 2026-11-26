[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [Parameter(Mandatory = $true)][string]$ProtocExe,
    [Parameter(Mandatory = $true)][string]$PythonExe,
    [Parameter(Mandatory = $true)][string]$UvExe,
    [switch]$Generate
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
$generateArgs = @('-I', '-B', '-m', 'grpc_tools.protoc', "--proto_path=$protoRoot",
    "--python_out=$staged", "--pyi_out=$staged")
& $env:DD_WORKER_PYTHON @generateArgs (Join-Path $protoRoot 'datasetdoctor\v1\common.proto')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $env:DD_WORKER_PYTHON @generateArgs "--grpc_python_out=$staged" (Join-Path $protoRoot 'datasetdoctor\v1\worker_service.proto')
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
exit $LASTEXITCODE
