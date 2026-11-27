[CmdletBinding()]
param([Parameter(Mandatory)][string]$UvExe)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$before = $env:UV_NO_PYTHON_DOWNLOADS
$env:UV_NO_PYTHON_DOWNLOADS = '1'
try {
    $python = & (Join-Path $repo 'scripts/ci/prepare-python.ps1') -UvExe $UvExe -Offline
    if (-not (Test-Path -LiteralPath $python -PathType Leaf)) { throw 'Bootstrap did not return an actual interpreter.' }
    $actual = & $python -I -B -c "import json, sys, struct; print(json.dumps([sys.implementation.name, sys.version.split()[0], struct.calcsize('P') * 8]))"
    if ($LASTEXITCODE -ne 0) { throw 'Bootstrapped Python did not start.' }
    $details = $actual | ConvertFrom-Json
    $lock = Get-Content (Join-Path $repo 'config/toolchains/windows-x64.json') -Raw | ConvertFrom-Json
    if ($details[0] -cne 'cpython' -or "cpython $($details[1])" -cne $lock.python -or $details[2] -ne 64) {
        throw 'Bootstrap did not supply locked Windows x64 CPython.'
    }
    if ($env:UV_NO_PYTHON_DOWNLOADS -cne '1') { throw 'Bootstrap changed the acceptance download policy.' }
    Write-Output "CI_PINNED_PYTHON_BOOTSTRAP_PASSED $python"
} finally {
    $env:UV_NO_PYTHON_DOWNLOADS = $before
}
