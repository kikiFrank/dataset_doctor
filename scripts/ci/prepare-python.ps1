[CmdletBinding()]
param([string]$UvExe = 'uv', [switch]$Offline)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$lock = Get-Content (Join-Path $repo 'config/toolchains/windows-x64.json') -Raw | ConvertFrom-Json
$version = $lock.python -replace '^cpython ', ''
$request = "cpython-$version-windows-x86_64-none"
$arguments = @('python','install',$request,'--no-config','--no-progress')
if ($Offline) { $arguments += '--offline' }
# Only the prerequisite installation may download the pinned runtime. Restore
# the caller's no-download policy before dependency preparation or acceptance.
$previous = $env:UV_NO_PYTHON_DOWNLOADS
try {
    [Environment]::SetEnvironmentVariable('UV_NO_PYTHON_DOWNLOADS', $null, 'Process')
    & $UvExe @arguments | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not install locked Windows CPython.' }
} finally {
    $env:UV_NO_PYTHON_DOWNLOADS = $previous
}
$python = & $UvExe python find --managed-python --no-python-downloads --no-config $request
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $python -PathType Leaf)) {
    throw 'Locked managed Python is unavailable.'
}
$actual = & $python -I -B -c "import sys, struct; print('cpython ' + sys.version.split()[0] + ' ' + str(struct.calcsize('P') * 8))"
if ($LASTEXITCODE -ne 0 -or $actual -cne "$($lock.python) 64") { throw 'Locked Python version or architecture mismatch.' }
Write-Output $python
