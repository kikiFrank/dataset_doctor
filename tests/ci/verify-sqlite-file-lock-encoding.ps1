[CmdletBinding()]
param([Parameter(Mandatory)][string]$RustBin)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$original = [Console]::InputEncoding
try {
    # Match the hosted Windows UTF-8 console. Its BOM must not become part of
    # the unchanged P00 broker's first stdin handshake line.
    [Console]::InputEncoding = [Text.UTF8Encoding]::new($true)
    & (Join-Path $repo 'scripts/ci/run-sqlite-file-lock.ps1') -CargoExe (Join-Path $RustBin 'cargo.exe') -RustcExe (Join-Path $RustBin 'rustc.exe') -OfflineCargoHome (Join-Path $repo 'target/p00-03/cargo-home')
    if ($LASTEXITCODE -ne 0) { throw 'Actual P00 lock acceptance failed with the hosted console encoding.' }
    if ([Console]::InputEncoding.GetPreamble().Length -ne 3) { throw 'The caller console encoding was not restored.' }
    Write-Output 'CI_SQLITE_UTF8_CONSOLE_PASSED'
} finally { [Console]::InputEncoding = $original }
