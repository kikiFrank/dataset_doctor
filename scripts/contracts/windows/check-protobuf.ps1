[CmdletBinding()]
param(
    [string]$ProtocExe,
    [string]$GoogleProtoRoot,
    [string]$PythonExe
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$target = Join-Path $repo 'target'
$contractCheck = Join-Path $target 'contract-check'
$output = Join-Path $contractCheck 'p01-02'
$runner = Join-Path $repo 'tests\contracts\protobuf\artifact-ref\test_common_proto.py'
$utf8 = [Text.UTF8Encoding]::new($false)

function Initialize-OutputDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($item -and (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain output directory: $Path"
    }
    [void][IO.Directory]::CreateDirectory($Path)
}

function Write-CheckLog([string]$Line) {
    [IO.File]::AppendAllText($log, $Line + [Environment]::NewLine, $utf8)
    Write-Host $Line
}

function Resolve-CheckPath([string]$Path, [string]$Label, [bool]$Directory) {
    if ($Path -notmatch '^[A-Za-z]:[\\/]') { throw "Pass an absolute local path for $Label." }
    $item = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    if (-not $item -or $item.PSIsContainer -ne $Directory) { throw "$Label is unavailable: $Path" }
    return $item.FullName
}

function Invoke-LoggedCommand([string]$File, [string[]]$Arguments) {
    Write-CheckLog ('> ' + $File + ' ' + (($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '))
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = $null
    & $File @Arguments 2>&1 | ForEach-Object {
        if ($_ -is [Management.Automation.ErrorRecord]) { Write-CheckLog $_.Exception.Message }
        else { Write-CheckLog $_.ToString() }
    }
    $code = $global:LASTEXITCODE
    if ($null -eq $code) { throw "Could not start: $File" }
    Write-CheckLog "exit_code=$code"
    return $code
}

try {
    foreach ($directory in @($target, $contractCheck, $output)) { Initialize-OutputDirectory $directory }
    $log = Join-Path $output 'check.log'
    $existingLog = Get-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue
    if ($existingLog -and ($existingLog.PSIsContainer -or ($existingLog.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain check log: $log"
    }
    [IO.File]::WriteAllText($log, '', $utf8)
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}

try {
    $protoc = Resolve-CheckPath $ProtocExe 'protoc' $false
    $include = Resolve-CheckPath $GoogleProtoRoot 'Google Protobuf include directory' $true
    $python = Resolve-CheckPath $PythonExe 'Python interpreter' $false
    $code = Invoke-LoggedCommand $python @('-I', '-S', '-B', '--version')
    if ($code -ne 0) { throw "Python version probe failed with exit $code." }
} catch {
    Write-CheckLog "[result] PREREQUISITE FAILED: $($_.Exception.Message)"
    exit 2
}

try {
    $code = Invoke-LoggedCommand $python @(
        '-I', '-S', '-B', '-u', $runner, '--protoc', $protoc, '--google-proto-root', $include
    )
    if ($code -notin @(0, 1, 2)) { throw "Contract check process exited unexpectedly with code $code." }
    exit $code
} catch {
    Write-CheckLog "[result] CHECK FAILED: $($_.Exception.Message)"
    exit 1
}
