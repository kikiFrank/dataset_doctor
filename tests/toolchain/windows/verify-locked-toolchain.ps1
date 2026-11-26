[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string[]]$ToolDirectories,
    [Parameter(Mandatory = $true)][string]$AlternateUvDirectory
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$preflight = Join-Path $repo 'scripts\check-toolchain.ps1'
$powershell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
$originalPath = $env:PATH
try {
    $env:PATH = (@($ToolDirectories) + @($originalPath)) -join ';'
    $unlocked = @(& $powershell -NoProfile -NonInteractive -File $preflight)
    if ($LASTEXITCODE -ne 0) { throw "Version discovery failed: $($unlocked -join [Environment]::NewLine)" }
    $locked = @(& $powershell -NoProfile -NonInteractive -File $preflight -Locked)
    $code = $LASTEXITCODE
    if ($code -ne 0 -or $locked -notcontains 'Locked toolchain: windows-x64') {
        throw "The locked toolchain was not verified: $($locked -join [Environment]::NewLine)"
    }
    $locked

    $env:PATH = "$AlternateUvDirectory;$env:PATH"
    $unlocked = @(& $powershell -NoProfile -NonInteractive -File $preflight)
    if ($LASTEXITCODE -ne 0 -or -not ($unlocked -match '^uv: OK ')) {
        throw "The alternate uv executable was not verified: $($unlocked -join [Environment]::NewLine)"
    }
    $mismatch = @(& $powershell -NoProfile -NonInteractive -File $preflight -Locked)
    if ($LASTEXITCODE -ne 1 -or -not ($mismatch -match '^uv: VERSION MISMATCH .* Expected 0\.9\.26\.')) {
        throw "Expected the exact uv pin to reject the alternate version: $($mismatch -join [Environment]::NewLine)"
    }
    $mismatch -match '^uv: VERSION MISMATCH '
    'Locked toolchain accepts the pinned installation and rejects version drift.'
} finally {
    $env:PATH = $originalPath
}
