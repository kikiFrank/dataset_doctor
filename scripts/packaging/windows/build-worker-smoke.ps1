[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PythonExe,
    [Parameter(Mandatory = $true)][string]$UvCacheDir
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$project = Join-Path $repo 'packaging\windows\worker-smoke'
$dist = Join-Path $repo 'dist'
$root = Join-Path $dist 'p00-worker-smoke'
$logs = Join-Path $root 'logs'
$venv = Join-Path $root 'venv'
$build = Join-Path $root 'build'
$spec = Join-Path $root 'spec'
$package = Join-Path $root 'package'
$temp = Join-Path $root 'temp'
$bundle = Join-Path $package 'worker-smoke'
$utf8 = [Text.UTF8Encoding]::new($false)

function Initialize-BuildDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($item -and (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain build directory: $Path"
    }
    [void][IO.Directory]::CreateDirectory($Path)
}

function Write-BuildLog([string]$Line) {
    [IO.File]::AppendAllText($log, $Line + [Environment]::NewLine, $utf8)
    Write-Host $Line
}

function Invoke-LoggedNative([string]$File, [string[]]$Arguments) {
    Write-BuildLog ('> ' + $File + ' ' + (($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '))
    $ErrorActionPreference = 'Continue'
    & $File @Arguments 2>&1 | ForEach-Object { Write-BuildLog $_.ToString() }
    $code = $LASTEXITCODE
    Write-BuildLog "exit_code=$code"
    if ($code -ne 0) { throw "Command exited with code $code. See $log" }
}

$log = $null
$savedEnvironment = @{}
try {
    foreach ($directory in @($dist, $root, $logs)) { Initialize-BuildDirectory $directory }
    $logPath = Join-Path $logs 'build.log'
    $existingLog = Get-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
    if ($existingLog -and ($existingLog.PSIsContainer -or ($existingLog.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain log file: $logPath"
    }
    [IO.File]::WriteAllText($logPath, '', $utf8)
    $log = $logPath
    Write-BuildLog "Windows Worker smoke build on $([Environment]::OSVersion.VersionString) ($env:PROCESSOR_ARCHITECTURE)"
    if ($PythonExe -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $PythonExe -PathType Leaf)) {
        throw 'Pass the absolute path to the installed CPython 3.12.12 x64 executable.'
    }
    $python = Get-Item -LiteralPath $PythonExe
    if (($python.Attributes -bor $python.Directory.Attributes) -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Pass the physical CPython executable path outside version alias directories.'
    }
    if ($UvCacheDir -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $UvCacheDir -PathType Container)) {
        throw 'Pass the absolute path to an existing, approved local uv cache.'
    }
    $cache = (Get-Item -LiteralPath $UvCacheDir).FullName
    $uv = (Get-Command uv.exe -CommandType Application | Select-Object -First 1).Source

    $checkPython = "import sys, sysconfig; print(sys.version); print(sysconfig.get_platform()); sys.exit(0 if sys.implementation.name == 'cpython' and sys.version_info[:3] == (3, 12, 12) and sysconfig.get_platform() == 'win-amd64' else 1)"
    Invoke-LoggedNative $python.FullName @('-I', '-S', '-B', '-c', $checkPython)
    Invoke-LoggedNative $uv @('--version')
    if (-not (Test-Path -LiteralPath (Join-Path $project 'uv.lock') -PathType Leaf)) {
        throw 'uv.lock is missing. Run the README command for offline dependency locking using an approved cache.'
    }

    foreach ($directory in @($venv, $build, $spec, $package, $temp)) { Initialize-BuildDirectory $directory }
    if (Test-Path -LiteralPath $bundle) { Initialize-BuildDirectory $bundle }
    $config = Join-Path $build 'pyinstaller-config'
    Initialize-BuildDirectory $config
    $environment = @{
        UV_PROJECT_ENVIRONMENT = $venv
        UV_LINK_MODE = 'copy'
        PYINSTALLER_CONFIG_DIR = $config
        TEMP = $temp
        TMP = $temp
    }
    # The build uses the supplied cache and committed lock in this process only.
    foreach ($entry in (Get-ChildItem Env: | Where-Object Name -Like 'UV_*')) {
        $savedEnvironment[$entry.Name] = $entry.Value
        [Environment]::SetEnvironmentVariable($entry.Name, $null)
    }
    foreach ($name in $environment.Keys) {
        if (-not $savedEnvironment.ContainsKey($name)) {
            $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
        }
        [Environment]::SetEnvironmentVariable($name, $environment[$name])
    }

    Invoke-LoggedNative $uv @(
        'sync', '--locked', '--offline', '--no-config', '--no-python-downloads', '--no-progress',
        '--no-build', '--prerelease', 'disallow', '--keyring-provider', 'disabled',
        '--default-index', 'https://pypi.org/simple',
        '--python', $python.FullName, '--cache-dir', $cache, '--project', $project
    )
    $buildPython = Join-Path $venv 'Scripts\python.exe'
    $versions = "import importlib.metadata as m; print('PyInstaller ' + m.version('PyInstaller')); print('Pillow ' + m.version('Pillow'))"
    Invoke-LoggedNative $buildPython @('-I', '-B', '-c', $versions)
    Invoke-LoggedNative $buildPython @(
        '-I', '-B', '-m', 'PyInstaller', '--onedir', '--console', '--noupx', '--noconfirm',
        '--name', 'worker-smoke', '--workpath', $build, '--specpath', $spec, '--distpath', $package,
        (Join-Path $project 'worker_smoke.py')
    )
    $exe = Join-Path $bundle 'worker-smoke.exe'
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing build output: $exe" }
    Write-BuildLog "Built $exe"
    exit 0
} catch {
    if ($log) { Write-BuildLog "Build failed: $($_.Exception.Message)" }
    else { [Console]::Error.WriteLine($_.Exception.Message) }
    exit 1
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name])
    }
}
