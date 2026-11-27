[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$DesktopExe,
    [ValidateSet('Runtime', 'Workspace', 'WorkspaceCreate', 'WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow', 'WorkspaceWorkflowControls', 'WorkspaceReportExport', 'WorkspaceStagingBatch', 'WorkspaceVideo')][string]$Suite = 'Runtime',
    [string]$PythonExe,
    [string]$PackagedWorkerExe,
    [string]$StagingFixturePlan,
    [string]$VideoToolsEvidence
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
    if ($PackagedWorkerExe) {
        if ($Suite -notin @('WorkspaceBusiness', 'WorkspaceReportExport', 'WorkspaceColor', 'WorkspaceVideo')) {
            throw 'The chosen suite does not support a packaged Worker layout.'
        }
        $product = (Resolve-Path -LiteralPath $PackagedWorkerExe).Path
        if ((Split-Path -Leaf $product) -ne 'dd-worker.exe') { throw 'Expected the packaged dd-worker.exe.' }
        $appFolder = Join-Path $run 'Packaged desktop'
        New-Item -ItemType Directory -Path $appFolder | Out-Null
        $copy = Join-Path $appFolder 'Dataset Doctor.exe'
        Copy-Item -LiteralPath $exe -Destination $copy
        $workerFolder = Join-Path $appFolder 'dd-worker'
        Copy-Item -LiteralPath (Split-Path -Parent $product) -Destination $workerFolder -Recurse
        $workerDigest = (Get-FileHash -LiteralPath $product).Hash
        if ((Get-FileHash -LiteralPath (Join-Path $workerFolder 'dd-worker.exe')).Hash -ne $workerDigest) {
            throw 'Worker executable changed while copying the product layout.'
        }
        $start.FileName = $copy
        $start.EnvironmentVariables['DD_WORKER_PYTHON'] = Join-Path $run 'missing-python.exe'
        $start.EnvironmentVariables['DD_WORKER_ENTRYPOINT'] = Join-Path $run 'missing-worker.py'
    }
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
    } elseif ($Suite -eq 'WorkspaceVideo') {
        if (-not $PackagedWorkerExe -or -not $VideoToolsEvidence) {
            throw 'Pass the packaged Worker executable and the verified FFmpeg toolchain evidence.'
        }
        $python = (Resolve-Path -LiteralPath $PythonExe).Path
        $video = & (Join-Path $PSScriptRoot 'seed_packaged_video.ps1') -Run $run -WorkerFolder $workerFolder -VideoToolsEvidence $VideoToolsEvidence
        $workspace = Join-Path $run 'Synthetic video Workspace'
        New-Item -ItemType Directory -Path $workspace | Out-Null
        $start.Arguments += ' --workspace-video-smoke'
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_WORKSPACE'] = $workspace
        $start.EnvironmentVariables['DD_WORKER_VIDEO_FFMPEG'] = Join-Path $run 'missing-ffmpeg.exe'
        $start.EnvironmentVariables['DD_WORKER_VIDEO_FFPROBE'] = Join-Path $run 'missing-ffprobe.exe'
        $expected = 'DESKTOP_WORKSPACE_VIDEO_VERIFIED'
    } elseif ($Suite -eq 'WorkspaceStagingBatch') {
        $python = (Resolve-Path -LiteralPath $PythonExe).Path
        if (-not $StagingFixturePlan) { throw 'Pass a staging-batch-ui.json from the video command host.' }
        $fixturePlan = (Resolve-Path -LiteralPath $StagingFixturePlan).Path
        $testRuns = Join-Path $repo 'target\desktop-commands\runs'
        $prefix = $testRuns + '\'
        if (-not $fixturePlan.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Staging fixture plan must be under the desktop command runs.'
        }
        $plan = Get-Content -LiteralPath $fixturePlan -Encoding utf8 -Raw | ConvertFrom-Json
        $fixtureWorkspace = (Resolve-Path -LiteralPath $plan.workspace).ProviderPath.Replace('\\?\', '')
        $fixtureVideo = (Resolve-Path -LiteralPath $plan.source_video).ProviderPath.Replace('\\?\', '')
        $fixtureRun = (Split-Path -Parent $fixturePlan) + '\'
        if (-not $fixtureWorkspace.StartsWith($fixtureRun, [StringComparison]::OrdinalIgnoreCase) -or
            -not $fixtureVideo.StartsWith($fixtureRun, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Staging fixture files must belong to the command host run.'
        }
        $sourceDigest = (Get-FileHash -LiteralPath $fixtureVideo).Hash
        $workspace = Join-Path $run 'Staging Batch Workspace'
        Copy-Item -LiteralPath $fixtureWorkspace -Destination $workspace -Recurse
        $start.Arguments += ' --workspace-staging-batch-smoke'
        $start.EnvironmentVariables['DD_DESKTOP_SMOKE_WORKSPACE'] = $workspace
        $expected = 'DESKTOP_WORKSPACE_STAGING_BATCH_VERIFIED'
    } elseif ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow', 'WorkspaceWorkflowControls', 'WorkspaceReportExport')) {
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
        } elseif ($Suite -eq 'WorkspaceWorkflowControls') {
            $start.Arguments += ' --workspace-workflow-controls-smoke'
            $expected = 'DESKTOP_WORKSPACE_WORKFLOW_CONTROLS_VERIFIED'
        } elseif ($Suite -eq 'WorkspaceReportExport') {
            $start.Arguments += ' --workspace-report-export-smoke'
            $expected = 'DESKTOP_WORKSPACE_REPORT_EXPORT_VERIFIED'
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
        $timeout = if ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor', 'WorkspaceWorkflow', 'WorkspaceWorkflowControls', 'WorkspaceReportExport', 'WorkspaceStagingBatch', 'WorkspaceVideo')) { 55000 } else { 25000 }
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
        if ($Suite -eq 'WorkspaceVideo') {
            & $python -I -B (Join-Path $PSScriptRoot 'verify_video_fixture.py') $workspace $video.Source $video.Reference $video.SourceDigest
            if ($LASTEXITCODE -ne 0) { throw 'The packaged video failed the database, index and frame checks.' }
        } elseif ($Suite -eq 'WorkspaceStagingBatch') {
            & $python -I -B (Join-Path $PSScriptRoot 'verify_staging_batch_fixture.py') $workspace $fixturePlan $sourceDigest
            if ($LASTEXITCODE -ne 0) { throw 'The Staging batch failed the database and source checks.' }
        } elseif ($Suite -in @('WorkspaceWorkflow', 'WorkspaceWorkflowControls', 'WorkspaceReportExport')) {
            $arguments = @($business)
            if ($Suite -eq 'WorkspaceReportExport') { $arguments += '--exported' }
            if ($Suite -eq 'WorkspaceWorkflowControls') { $arguments += '--controlled' }
            & $python -I -B (Join-Path $PSScriptRoot 'verify_workflow_fixture.py') @arguments
            if ($LASTEXITCODE -ne 0) { throw 'The local workflow failed the database and report checks.' }
        } elseif ($Suite -in @('WorkspaceBusiness', 'WorkspaceColor')) {
            $packages = @(Get-ChildItem -LiteralPath (Join-Path $business 'Exports') -Directory)
            if ($packages.Count -ne 1) { throw 'Expected exactly one published COCO package.' }
            $arguments = @($business, $packages[0].FullName)
            if ($Suite -eq 'WorkspaceColor') { $arguments += '--color' }
            & $python -I -B (Join-Path $PSScriptRoot 'verify_business_fixture.py') @arguments
            if ($LASTEXITCODE -ne 0) { throw 'The target COCO reader rejected the exported business fixture.' }
        }
        if ($PackagedWorkerExe) { "Packaged Worker SHA-256 $workerDigest" | Add-Content -LiteralPath $log -Encoding UTF8 }
        "Native desktop $Suite completed Tauri IPC and exited: $log"
    } finally {
        $process.Dispose()
    }
} catch {
    $_.Exception.Message | Add-Content -LiteralPath $log -Encoding UTF8
    throw "Desktop runtime check failed. See $log. $($_.Exception.Message)"
}
