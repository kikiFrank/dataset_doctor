[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$RustBin)

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
$log = Join-Path $run 'check.log'
$args = @('test', '--manifest-path', (Join-Path $repo 'Cargo.toml'), '--locked', '--offline',
    '--target', 'x86_64-pc-windows-msvc', '-p', 'dd-desktop', 'workspace_commands::')
"Running desktop host tests without starting WebView2. Log: $log" | Tee-Object -FilePath $log
$ErrorActionPreference = 'Continue'
& $cargo @args 2>&1 | ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
$result = $LASTEXITCODE
"exit_code=$result" | Tee-Object -FilePath $log -Append
exit $result
