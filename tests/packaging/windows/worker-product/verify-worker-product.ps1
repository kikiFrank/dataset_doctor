[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $WorkerExe,
    [Parameter(Mandatory)][string] $RustBin,
    [Parameter(Mandatory)][string] $ProtocExe,
    [Parameter(Mandatory)][string] $PythonExe,
    [Parameter(Mandatory)][string] $UvExe,
    [string] $VideoFfmpegExe,
    [string] $VideoFfprobeExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../../..')).Path
if (-not (Test-Path -LiteralPath $WorkerExe -PathType Leaf)) {
    [Console]::Error.WriteLine("Missing production Worker executable: $WorkerExe")
    exit 1
}
$source = Get-Item -LiteralPath $WorkerExe
$staged = Join-Path $repo ('target/p10/worker-smoke/' + [guid]::NewGuid().ToString('N'))
New-Item -Path $staged -ItemType Directory | Out-Null
$installed = Join-Path $staged 'Installed Worker'
Copy-Item -LiteralPath $source.Directory.FullName -Destination $installed -Recurse
$exe = Join-Path $installed $source.Name
$check = Join-Path $repo 'scripts/contracts/windows/check-worker-rpc.ps1'
Write-Host "Verifying relocated Worker: $exe"
$arguments = @('-RustBin', $RustBin, '-ProtocExe', $ProtocExe,
    '-PythonExe', $PythonExe, '-UvExe', $UvExe, '-PackagedWorkerExe', $exe)
if ([bool]$VideoFfmpegExe -ne [bool]$VideoFfprobeExe) { throw 'Pass both video tool executables.' }
if ($VideoFfmpegExe) {
    $arguments += @('-VideoFfmpegExe', $VideoFfmpegExe, '-VideoFfprobeExe', $VideoFfprobeExe)
}
powershell.exe -NoProfile -NonInteractive -File $check @arguments
exit $LASTEXITCODE
