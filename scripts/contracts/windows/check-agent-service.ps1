[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [Parameter(Mandatory = $true)][string]$ProtocExe,
    [ValidateSet('All', 'Lifecycle', 'Model')][string]$Suite = 'All'
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
if ($env:VSCMD_ARG_TGT_ARCH -ne 'x64') { throw 'Initialize x64 VS Developer PowerShell.' }
$env:CARGO_HOME = (Resolve-Path -LiteralPath (Join-Path $repo 'target\dev\cargo-home')).Path
$env:CARGO_TARGET_DIR = Join-Path $repo 'target\dev\cargo-target'
$env:RUSTC = (Resolve-Path -LiteralPath (Join-Path $RustBin 'rustc.exe')).Path
$env:PROTOC = (Resolve-Path -LiteralPath $ProtocExe).Path
$cargo = (Resolve-Path -LiteralPath (Join-Path $RustBin 'cargo.exe')).Path
$run = Join-Path $repo "target\contract-check\agent-service\$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $run -Force | Out-Null
$log = Join-Path $run 'check.log'
"Local Agent acceptance suite=$Suite. Evidence: $log" | Tee-Object -FilePath $log

$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false
$tests = @()
if ($Suite -in @('All', 'Lifecycle')) { $tests += @('--test', 'agent_service') }
if ($Suite -in @('All', 'Model')) { $tests += @('--test', 'rig_model_client') }
& $cargo test --manifest-path (Join-Path $repo 'Cargo.toml') --locked --offline --target x86_64-pc-windows-msvc -p dd-agent-service @tests -- --nocapture 2>&1 |
    ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
$result = $LASTEXITCODE
"exit_code=$result" | Tee-Object -FilePath $log -Append
exit $result
