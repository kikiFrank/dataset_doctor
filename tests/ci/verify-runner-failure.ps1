[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$configurationPath = Join-Path $repo 'target/ci/typescript/environment.json'
$original = [IO.File]::ReadAllBytes($configurationPath)
$configuration = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
if ($configuration.repo -cne $repo -or $configuration.check -cne 'typescript') {
    throw 'Prepare the TypeScript check in this repository before verifying failure handling.'
}
$run = Join-Path $repo ('target/ci/runner-failure/' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $run | Out-Null
$rejected = $false
try {
    $node = $configuration.node
    $configuration.pnpm = Join-Path $run 'missing-pnpm.exe'
    $configuration.node = Join-Path $run 'missing-node.exe'
    $configuration | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $configurationPath -Encoding utf8
    # Establish a real previous successful native exit, then remove the next
    # commands through their configured paths. No real executable is modified.
    & $node -e 'process.exit(0)'
    if ($LASTEXITCODE -ne 0) { throw 'Could not establish the successful predecessor.' }
    try {
        & (Join-Path $repo 'scripts/ci/run-windows-check.ps1') -Check typescript *> (Join-Path $run 'probe.log')
    } catch {
        if ($_.Exception.Message -notmatch 'CI check command') { throw }
        $rejected = $true
        $_.Exception.Message | Add-Content -LiteralPath (Join-Path $run 'probe.log')
    }
} finally {
    [IO.File]::WriteAllBytes($configurationPath, $original)
}
if (-not $rejected) { throw 'CI runner accepted missing executables using a stale successful exit code.' }
Write-Output "CI_MISSING_TOOL_FAILURE_REJECTED Evidence: $run"
