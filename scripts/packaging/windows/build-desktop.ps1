[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [Parameter(Mandatory = $true)][string]$PnpmExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
if ($env:VSCMD_ARG_TGT_ARCH -ne 'x64') { throw 'Initialize x64 VS Developer PowerShell.' }
$desktop = Join-Path $repo 'apps\desktop'
$dev = Join-Path $repo 'target\dev'
$store = Join-Path $dev 'pnpm-store'
$pnpm = (Resolve-Path -LiteralPath $PnpmExe).Path
$cargo = (Resolve-Path -LiteralPath (Join-Path $RustBin 'cargo.exe')).Path
$env:RUSTC = (Resolve-Path -LiteralPath (Join-Path $RustBin 'rustc.exe')).Path
$env:CARGO_HOME = (Resolve-Path -LiteralPath (Join-Path $dev 'cargo-home')).Path
$env:CARGO_TARGET_DIR = Join-Path $dev 'cargo-target'
$manifest = Join-Path $repo 'Cargo.toml'
$target = 'x86_64-pc-windows-msvc'

Push-Location -LiteralPath $desktop
try {
    & $pnpm install --store-dir $store --frozen-lockfile --offline --ignore-scripts
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & $pnpm run build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally { Pop-Location }

& $cargo build --manifest-path $manifest --locked --offline --target $target -p dd-desktop
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path $env:CARGO_TARGET_DIR "$target\debug\dd-desktop.exe"
Write-Output "Desktop executable: $exe"
