[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [Parameter(Mandatory = $true)][string]$NodeExe,
    [switch]$Generate
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$out = Join-Path $repo "target\contract-check\ui-wire\$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $out -Force | Out-Null
$candidate = Join-Path $out 'wire-primitives.ts'
$tracked = Join-Path $repo 'apps\desktop\src\api\generated\wire-primitives.ts'
$env:CARGO_HOME = (Resolve-Path -LiteralPath (Join-Path $repo 'target\dev\cargo-home')).Path
$env:CARGO_TARGET_DIR = Join-Path $repo 'target\dev\cargo-target'
$env:RUSTC = Join-Path $RustBin 'rustc.exe'
$cargo = Join-Path $RustBin 'cargo.exe'
$cargoArgs = @('--manifest-path', (Join-Path $repo 'Cargo.toml'), '--locked', '--offline',
    '--target', 'x86_64-pc-windows-msvc', '-p', 'dd-ui-contracts')
if ($env:DD_CARGO_DEADLINE_CONFIG) { $cargoArgs += @('--config',$env:DD_CARGO_DEADLINE_CONFIG) }

& $cargo run @cargoArgs --bin export-ui-contracts -- $candidate
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Generate) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $tracked) -Force | Out-Null
    Copy-Item -LiteralPath $candidate -Destination $tracked
}
if ((Get-FileHash -LiteralPath $candidate).Hash -cne (Get-FileHash -LiteralPath $tracked).Hash) {
    throw 'Generated TypeScript declarations differ from Rust DTOs.'
}

$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false
$log = Join-Path $out 'check.log'
"Generated TypeScript declarations match. Evidence: $out" | Tee-Object -FilePath $log
& $cargo test @cargoArgs --test ui_wire_primitives 2>&1 |
    ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$nodeArgs = @('--test','--test-timeout=30000',(Join-Path $repo 'tests\contracts\typescript\ui-wire-primitives.test.mjs'))
if ($env:DD_TEST_PYTHON) {
    & $env:DD_TEST_PYTHON -I -B (Join-Path $repo 'scripts/testing/bounded_test.py') -- $NodeExe @nodeArgs 2>&1 |
        ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
} else { & $NodeExe @nodeArgs 2>&1 |
    ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
}
exit $LASTEXITCODE
