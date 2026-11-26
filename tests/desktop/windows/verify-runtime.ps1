[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$DesktopExe,
    [ValidateSet('Runtime', 'Workspace', 'WorkspaceCreate', 'WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow')][string]$Suite = 'Runtime',
    [string]$PythonExe
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$run = Join-Path $repo "target\desktop-smoke\runs\$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $run -Force | Out-Null
$log = Join-Path $run 'verify.log'
try {
    $exe = (Resolve-Path -LiteralPath $DesktopExe).Path
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $exe
    $start.Arguments = '--runtime-smoke'
    $start.WorkingDirectory = $run
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['DD_DESKTOP_SMOKE_ROOT'] = $run
    $expected = 'DESKTOP_WEBVIEW2_READY'
    if ($Suite -eq 'Workspace') {
        $python = (Resolve-Path -LiteralPath $PythonExe).Path
        $workspace = Join-Path $run "$([char]0x9a8c)$([char]0x6536) Workspace"
        & $python -I -S -B (Join-Path $PSScriptRoot 'seed_workspace.py') $workspace
        if ($LASTEXITCODE -ne 0) { throw 'Synthetic Workspace initialization failed.' }
        $start.Arguments += ' --workspace-smoke'
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_WORKSPACE'] = $workspace
        $expected = 'DESKTOP_WORKSPACE_VERIFIED'
    } elseif ($Suite -eq 'WorkspaceCreate') {
        $workspace = Join-Path $run 'New Workspace'
        New-Item -ItemType Directory -Path $workspace | Out-Null
        $start.Arguments += ' --workspace-create-smoke'
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_WORKSPACE'] = $workspace
        $expected = 'DESKTOP_WORKSPACE_CREATE_VERIFIED'
    } elseif ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow')) {
        $python = (Resolve-Path -LiteralPath $PythonExe).Path
        $business = Join-Path $run 'Synthetic business'
        & $python -I -B (Join-Path $PSScriptRoot 'seed_business_fixture.py') $business
        if ($LASTEXITCODE -ne 0) { throw 'Synthetic business fixture initialization failed.' }
        if ($Suite -eq 'WorkspaceColor') {
            $start.Arguments += ' --workspace-color-smoke'
            $expected = 'DESKTOP_WORKSPACE_COLOR_VERIFIED'
        } elseif ($Suite -eq 'WorkspaceWorkflow') {
            $start.Arguments += ' --workspace-workflow-smoke'
            $expected = 'DESKTOP_WORKSPACE_WORKFLOW_VERIFIED'
        } else {
            $start.Arguments += ' --workspace-business-smoke'
            $expected = 'DESKTOP_WORKSPACE_BUSINESS_VERIFIED'
        }
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_ROOT'] = $business
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_WORKSPACE'] = Join-Path $business 'Workspace'
    }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $timeout = if ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow')) { 55000 } else { 25000 }
        if (-not $process.WaitForExit($timeout)) {
            $process.Kill()
            throw 'The native desktop did not acknowledge the WebView2 IPC handshake.'
        }
        if (-not $stdout.Wait(2000) -or -not $stderr.Wait(2000)) {
            throw 'The desktop process did not close its output streams.'
        }
        $stdout.Result, $stderr.Result | Set-Content -LiteralPath $log -Encoding UTF8
        if ($process.ExitCode -ne 0 -or $stdout.Result -notmatch "(?m)^$expected version=0\.1\.0\s*$") {
            throw "Desktop exit=$($process.ExitCode). Missing $expected. See $log."
        }
        if ($Suite -eq 'WorkspaceCreate' -and (
            -not (Test-Path -LiteralPath (Join-Path $workspace 'workspace.json') -PathType Leaf) -or
            -not (Test-Path -LiteralPath (Join-Path $workspace 'workspace.db') -PathType Leaf))) {
            throw 'The native application did not publish both Workspace files.'
        }
        if ($Suite -eq 'WorkspaceWorkflow') {
            & $python -I -B (Join-Path $PSScriptRoot 'verify_workflow_fixture.py') $business
            if ($LASTEXITCODE -ne 0) { throw 'The local workflow failed the database and report checks.' }
        } elseif ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor')) {
            $packages = @(Get-ChildItem -LiteralPath (Join-Path $business 'Exports') -Directory)
            if ($packages.Count -ne 1) { throw 'Expected exactly one published COCO package.' }
            $arguments = @($business, $packages[0].FullName)
            if ($Suite -eq 'WorkspaceColor') { $arguments += '--color' }
            & $python -I -B (Join-Path $PSScriptRoot 'verify_business_fixture.py') @arguments
            if ($LASTEXITCODE -ne 0) { throw 'The target COCO reader rejected the exported business fixture.' }
        }
        "Native desktop $Suite completed Tauri IPC and exited: $log"
    } finally {
        $process.Dispose()
    }
} catch {
    $_.Exception.Message | Add-Content -LiteralPath $log -Encoding UTF8
    throw "Desktop runtime check failed. See $log. $($_.Exception.Message)"
}
