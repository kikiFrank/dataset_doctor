[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $PythonExe,
    [Parameter(Mandatory)][string] $UvExe,
    [string] $UvCacheDir,
    [string] $PyInstallerCacheDir
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$worker = Join-Path $repo 'services/python-worker'
$source = Join-Path $worker 'src/dd_worker'
$packaging = Join-Path $repo 'packaging/windows/worker-smoke'
$python = (Resolve-Path -LiteralPath $PythonExe).Path
$uv = (Resolve-Path -LiteralPath $UvExe).Path
if (-not $UvCacheDir) { $UvCacheDir = Join-Path $repo 'target/dev/uv-cache' }
if (-not $PyInstallerCacheDir) { $PyInstallerCacheDir = Join-Path $repo 'dist/p00-worker-smoke/uv-cache' }
$cache = (Resolve-Path -LiteralPath $UvCacheDir).Path
$toolCache = (Resolve-Path -LiteralPath $PyInstallerCacheDir).Path
$run = Join-Path $repo ('target/p10/worker-product/' + [guid]::NewGuid().ToString('N'))
$out = Join-Path $run 'package'
$env:UV_PROJECT_ENVIRONMENT = Join-Path $run 'runtime'
$env:PYINSTALLER_CONFIG_DIR = Join-Path $run 'pyinstaller-cache'
$env:TEMP = Join-Path $run 'temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Force -Path $out, $env:TEMP | Out-Null
$log = Join-Path $run 'build.log'

function Invoke-BuildStep([string] $Executable, [string[]] $Arguments) {
    $ErrorActionPreference = 'Continue'
    $PSNativeCommandUseErrorActionPreference = $false
    & $Executable @Arguments 2>&1 | ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
    if ($LASTEXITCODE -ne 0) { throw "Build step failed with exit code $LASTEXITCODE. See $log" }
}

$checkPython = "import sys,sysconfig; print(sys.version, sysconfig.get_platform()); assert sys.implementation.name == 'cpython' and sys.version_info[:3] == (3,12,12) and sysconfig.get_platform() == 'win-amd64'"
Invoke-BuildStep $python @('-I','-B','-c',$checkPython)
Invoke-BuildStep $uv @('sync','--locked','--offline','--no-build','--no-dev','--no-python-downloads',
    '--no-config','--project',$worker,'--python',$python,'--cache-dir',$cache)
$toolRequirements = Join-Path $run 'pyinstaller-requirements.txt'
Invoke-BuildStep $uv @('export','--locked','--offline','--no-config','--no-dev','--no-emit-project',
    '--prerelease','disallow','--cache-dir',$toolCache,'--default-index','https://pypi.org/simple',
    '--format','requirements-txt','--project',$packaging,'--output-file',$toolRequirements)
$buildPython = Join-Path $env:UV_PROJECT_ENVIRONMENT 'Scripts/python.exe'
Invoke-BuildStep $uv @('pip','install','--offline','--no-build','--no-deps','--require-hashes','--no-config',
    '--python',$buildPython,'--cache-dir',$toolCache,'--default-index','https://pypi.org/simple',
    '--requirements',$toolRequirements)
Invoke-BuildStep $buildPython @('-I','-B','-c',"import PyInstaller; print(PyInstaller.__version__); assert PyInstaller.__version__ == '6.18.0'")
Invoke-BuildStep $buildPython @('-I','-B','-m','PyInstaller','--onedir','--console','--noupx','--noconfirm',
    '--name','dd-worker','--workpath',(Join-Path $run 'build'),'--specpath',$run,'--distpath',$out,
    '--paths',$source,'--paths',(Join-Path $source 'generated'),
    (Join-Path $source 'entrypoint.py'))
$exe = Join-Path $out 'dd-worker/dd-worker.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing production Worker output: $exe" }
[ordered]@{
    worker_exe = $exe
    worker_exe_sha256 = (Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant()
    runtime_lock_sha256 = (Get-FileHash -LiteralPath (Join-Path $worker 'uv.lock')).Hash.ToLowerInvariant()
    packaging_lock_sha256 = (Get-FileHash -LiteralPath (Join-Path $packaging 'uv.lock')).Hash.ToLowerInvariant()
    pyinstaller_version = '6.18.0'
    python_version = '3.12.12'
    platform = 'win-amd64'
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'build-evidence.json') -Encoding UTF8
Write-Host "Production Worker: $exe"
