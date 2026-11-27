[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$CargoExe,
    [Parameter(Mandatory)][string]$RustcExe,
    [Parameter(Mandatory)][string]$OfflineCargoHome
)
$ErrorActionPreference = 'Stop'
$original = [Console]::InputEncoding
try {
    # .NET Framework's redirected stdin inherits Console.InputEncoding,
    # including its preamble. P00 deliberately requires an exact 'run' line.
    [Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
    & (Join-Path $PSScriptRoot '../storage/windows/check-sqlite-file-lock.ps1') @PSBoundParameters
    if ($LASTEXITCODE -ne 0) { throw "P00 SQLite lock acceptance exited with code $LASTEXITCODE" }
} finally { [Console]::InputEncoding = $original }
