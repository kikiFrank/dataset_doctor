[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('rust','typescript','python','contracts','sqlite','integration','desktop')][string]$Check,
    [switch]$LaunchDesktop
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Set-Location -LiteralPath $repo
$out = Join-Path $repo "target/ci/$Check"
$configuration = Get-Content -LiteralPath (Join-Path $out 'environment.json') -Raw | ConvertFrom-Json
if ($configuration.check -cne $Check -or $configuration.repo -cne $repo) { throw 'Prepare this check in the current repository first.' }
function Run([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = $null
    try {
        & $Exe @Arguments 2>&1 | ForEach-Object { $_.ToString() } |
            Tee-Object -FilePath (Join-Path $out 'commands.log') -Append -ErrorAction Stop
    } catch { throw "CI check command could not start or record output: $([IO.Path]::GetFileName($Exe))" }
    $code = $LASTEXITCODE
    if ($null -eq $code) { throw 'CI check command did not return an exit code.' }
    if ($code -ne 0) { throw "CI check command exited with code $code" }
}
function Script([string]$Path, [string[]]$Arguments) {
    Run 'powershell.exe' (@('-NoProfile','-NonInteractive','-File',(Join-Path $repo $Path)) + $Arguments)
}
if ($configuration.rust_bin) {
    if ($env:VSCMD_ARG_TGT_ARCH -ne 'x64') {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
        $visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($LASTEXITCODE -ne 0 -or -not $visualStudio) { throw 'Windows x64 MSVC tools are required.' }
        & (Join-Path $visualStudio 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
    }
    $cargo = Join-Path $configuration.rust_bin 'cargo.exe'
    $env:PATH = $configuration.rust_bin + [IO.Path]::PathSeparator + $env:PATH
    $env:RUSTC = Join-Path $configuration.rust_bin 'rustc.exe'
    $env:CARGO_HOME = Join-Path $repo 'target/dev/cargo-home'
    $env:CARGO_TARGET_DIR = Join-Path $repo 'target/dev/cargo-target'
    $env:CARGO_BUILD_JOBS = '4'
    $env:PROTOC = $configuration.protoc
    $cargoArgs = @('--locked','--offline','--target','x86_64-pc-windows-msvc')
}
Start-Transcript -LiteralPath (Join-Path $out 'check.log') -Force | Out-Null
try {
    switch ($Check) {
        'rust' {
            Run $cargo @('fmt','--all','--','--check')
            Run $cargo (@('clippy') + $cargoArgs + @('--workspace','--exclude','dd-desktop','--all-targets','--all-features','--','-D','warnings'))
            Run $cargo (@('test') + $cargoArgs + @('-p','dd-core','-p','dd-domain'))
        }
        'typescript' {
            Run $configuration.pnpm @('--dir','apps/desktop','run','build')
            Run $configuration.node @('--experimental-strip-types','--test','tests/desktop/ui/*.test.mjs','tests/ci/*.test.mjs')
        }
        'python' {
            $python = Join-Path $repo 'target/dev/worker-venv/Scripts/python.exe'
            $env:DD_TEST_FFMPEG = $configuration.ffmpeg
            $env:DD_TEST_FFPROBE = $configuration.ffprobe
            $videoRun = Join-Path $out ([guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Path $videoRun | Out-Null
            $env:DD_TEST_VIDEO_RUN = $videoRun
            $syntax = @'
from pathlib import Path
for root in (Path("services/python-worker/src"), Path("tests/worker")):
    for source in sorted(root.rglob("*.py")):
        compile(source.read_bytes(), str(source), "exec")
print("Worker Python syntax checks passed without generating bytecode")
'@
            Run $python @('-I','-B','-c',$syntax)
            foreach ($suite in @('image','annotation','dataset','quality','color','execution','cancellation','video')) {
                Run $python @('-I','-B','-m','unittest','discover','-s',"tests/worker/$suite",'-p','test_*.py','-v')
            }
        }
        'contracts' {
            Script 'scripts/contracts/windows/check-json-schema.ps1' @('-PythonExe',$configuration.python,'-UvCacheDir',$configuration.schema_cache)
            Script 'scripts/contracts/windows/check-protobuf.ps1' @('-ProtocExe',$configuration.protoc,'-GoogleProtoRoot',$configuration.google_proto_root,'-PythonExe',$configuration.python)
            Script 'scripts/contracts/windows/check-ui-wire.ps1' @('-RustBin',$configuration.rust_bin,'-NodeExe',$configuration.node)
            Script 'scripts/contracts/windows/check-worker-rpc.ps1' @('-RustBin',$configuration.rust_bin,'-ProtocExe',$configuration.protoc,'-PythonExe',$configuration.python,'-UvExe',$configuration.uv,'-StubsOnly')
        }
        'sqlite' {
            foreach ($suite in @('Connection','Migration','AppMigration','WorkspaceOpen','WorkspaceCreate','TaskStore')) {
                Script 'scripts/storage/windows/check-workspace-sqlite.ps1' @('-RustBin',$configuration.rust_bin,'-Suite',$suite)
            }
            Script 'scripts/storage/windows/check-sqlite-file-lock.ps1' @('-CargoExe',$cargo,'-RustcExe',$env:RUSTC,'-OfflineCargoHome',(Join-Path $repo 'target/p00-03/cargo-home'))
        }
        'integration' {
            & ./scripts/contracts/windows/check-worker-rpc.ps1 -RustBin $configuration.rust_bin -ProtocExe $configuration.protoc -PythonExe $configuration.python -UvExe $configuration.uv -VideoFfmpegExe $configuration.ffmpeg -VideoFfprobeExe $configuration.ffprobe -InfraTests @('core_claim_loopback','worker_geometry','worker_failures','workspace_geometry','review_final','review_write_authorization','task_cancellation','task_cancellation_video','workflow_review_runtime') 2>&1 |
                ForEach-Object { $_.ToString() } | Tee-Object -FilePath (Join-Path $out 'commands.log') -Append
            if ($LASTEXITCODE -ne 0) { throw 'Core/Worker integration acceptance failed.' }
        }
        'desktop' {
            Script 'scripts/packaging/windows/build-desktop.ps1' @('-RustBin',$configuration.rust_bin,'-PnpmExe',$configuration.pnpm)
            Run $cargo (@('clippy') + $cargoArgs + @('-p','dd-desktop','--all-targets','--features','video-acceptance','--','-D','warnings'))
            foreach ($filter in @('geometry_tests','review_tests','workflow_review_tests')) {
                $run = Join-Path $repo ('target/desktop-commands/runs/' + [guid]::NewGuid().ToString('N'))
                New-Item -ItemType Directory -Force -Path $run | Out-Null
                $env:DD_DESKTOP_COMMANDS_RUN_DIR = $run
                Run $cargo (@('test') + $cargoArgs + @('-p','dd-desktop','--bin','dd-desktop',"workspace_commands::$($filter)::"))
            }
            if (-not $LaunchDesktop) {
                'PRODUCT_CI_CHECK_DEFERRED desktop Runtime smoke requires desktop authorization.' |
                    Add-Content -LiteralPath (Join-Path $out 'commands.log')
                throw 'Desktop build/host checks passed. Runtime smoke is deferred. Pass -LaunchDesktop only when desktop automation is authorized.'
            }
            $exe = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/debug/dd-desktop.exe'
            Script 'tests/desktop/windows/verify-runtime.ps1' @('-DesktopExe',$exe)
            Script 'tests/desktop/windows/verify-runtime.ps1' @('-DesktopExe',$exe,'-Suite','WorkspaceCreate')
        }
    }
    Write-Output "PRODUCT_CI_CHECK_PASSED $Check"
} finally { Stop-Transcript | Out-Null }
