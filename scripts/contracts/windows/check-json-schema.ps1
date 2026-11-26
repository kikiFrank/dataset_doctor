[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PythonExe,
    [Parameter(Mandatory = $true)][string]$UvCacheDir,
    [ValidateSet('All', 'InputManifest', 'WorkspaceConfig')][string]$Suite = 'All'
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$project = Join-Path $repo 'tests\contracts\json-schema'
$dist = Join-Path $repo 'dist'
$artifacts = Join-Path $dist 'p01-01'
$logs = Join-Path $artifacts 'logs'
$venv = Join-Path $artifacts 'venv'
$temp = Join-Path $artifacts 'temp'
$utf8 = [Text.UTF8Encoding]::new($false)
$testPattern = switch ($Suite) {
    'InputManifest' { 'test_input_manifest.py' }
    'WorkspaceConfig' { 'test_workspace_config.py' }
    'All' { 'test_*.py' }
}

function Initialize-OutputDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($item -and (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain check directory: $Path"
    }
    [void][IO.Directory]::CreateDirectory($Path)
}

function Write-CheckLog([string]$Line) {
    [IO.File]::AppendAllText($log, $Line + [Environment]::NewLine, $utf8)
    Write-Host $Line
}

function Invoke-LoggedCommand([string]$File, [string[]]$Arguments) {
    Write-CheckLog ('> ' + $File + ' ' + (($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '))
    $ErrorActionPreference = 'Continue'
    & $File @Arguments 2>&1 | ForEach-Object {
        if ($_ -is [Management.Automation.ErrorRecord]) { Write-CheckLog $_.Exception.Message }
        else { Write-CheckLog $_.ToString() }
    }
    $code = $LASTEXITCODE
    Write-CheckLog "exit_code=$code"
    return $code
}

$log = $null
$savedEnvironment = @{}
try {
    foreach ($directory in @($dist, $artifacts, $logs)) { Initialize-OutputDirectory $directory }
    $logPath = Join-Path $logs 'check.log'
    $existingLog = Get-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
    if ($existingLog -and ($existingLog.PSIsContainer -or ($existingLog.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain check log file: $logPath"
    }
    [IO.File]::WriteAllText($logPath, '', $utf8)
    $log = $logPath

    if ($PythonExe -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $PythonExe -PathType Leaf)) {
        throw 'Pass the absolute path to the installed CPython 3.12.12 x64 executable.'
    }
    $python = Get-Item -LiteralPath $PythonExe
    if (($python.Attributes -bor $python.Directory.Attributes) -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Pass the physical Python executable path outside version alias directories.'
    }

    if ($UvCacheDir -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $UvCacheDir -PathType Container)) {
        throw 'Pass the absolute path to the existing P01-01 uv cache.'
    }
    $cache = Get-Item -LiteralPath $UvCacheDir
    if (-not $cache.FullName.StartsWith($artifacts + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "Use an approved cache under $artifacts."
    }
    for ($directory = $cache; $directory.FullName -ine $artifacts; $directory = $directory.Parent) {
        if ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "The cache directory must not use junctions or links: $($directory.FullName)"
        }
    }

    $uv = (Get-Command uv.exe -CommandType Application | Select-Object -First 1).Source
    $pythonCheck = "import sys, sysconfig; print(sys.version); print(sysconfig.get_platform()); sys.exit(0 if sys.implementation.name == 'cpython' and sys.version_info[:3] == (3, 12, 12) and sysconfig.get_platform() == 'win-amd64' else 1)"
    $code = Invoke-LoggedCommand $python.FullName @('-I', '-S', '-B', '-c', $pythonCheck)
    if ($code -ne 0) {
        Write-CheckLog 'Interpreter check failed. CPython 3.12.12 x64 (win-amd64) is required.'
        exit $code
    }
    $code = Invoke-LoggedCommand $uv @('--version')
    if ($code -ne 0) { exit $code }

    foreach ($directory in @($venv, $temp)) { Initialize-OutputDirectory $directory }
    foreach ($entry in (Get-ChildItem Env: | Where-Object Name -Like 'UV_*')) {
        $savedEnvironment[$entry.Name] = $entry.Value
        [Environment]::SetEnvironmentVariable($entry.Name, $null)
    }
    $environment = @{
        UV_PROJECT_ENVIRONMENT = $venv
        UV_LINK_MODE = 'copy'
        TEMP = $temp
        TMP = $temp
    }
    foreach ($name in $environment.Keys) {
        if (-not $savedEnvironment.ContainsKey($name)) {
            $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
        }
        [Environment]::SetEnvironmentVariable($name, $environment[$name])
    }

    $code = Invoke-LoggedCommand $uv @(
        'sync', '--locked', '--offline', '--no-config', '--no-python-downloads',
        '--no-build', '--prerelease', 'disallow', '--keyring-provider', 'disabled',
        '--default-index', 'https://pypi.org/simple', '--no-progress',
        '--python', $python.FullName, '--cache-dir', $cache.FullName, '--project', $project
    )
    if ($code -ne 0) { exit $code }

    $testPython = Join-Path $venv 'Scripts\python.exe'
    $versions = "from importlib.metadata import version; print('jsonschema ' + version('jsonschema'))"
    $code = Invoke-LoggedCommand $testPython @('-I', '-B', '-c', $versions)
    if ($code -ne 0) { exit $code }
    $code = Invoke-LoggedCommand $testPython @(
        '-I', '-B', '-m', 'unittest', 'discover', '-s', $project, '-p', $testPattern, '-v'
    )
    exit $code
} catch {
    if ($log) { Write-CheckLog "Schema check failed: $($_.Exception.Message)" }
    else { [Console]::Error.WriteLine($_.Exception.Message) }
    exit 1
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name])
    }
}
