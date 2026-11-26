[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RustBin,
    [string]$OfflineCargoHome,
    [ValidateSet('All', 'Connection', 'Migration', 'AppMigration', 'WorkspaceBackup', 'BackupAssets', 'BackupArtifacts', 'WorkspaceLock', 'WorkspaceOpen', 'WorkspaceAccess', 'VideoFileGrants', 'VideoRegistration', 'WorkspaceThread', 'WorkspaceCreate', 'ImportPreflight', 'ImportRegistration', 'ImportAuthorization', 'ImportPlan', 'DatasetBrowse', 'InputFingerprint', 'ReviewValidation', 'ReviewSession', 'AnnotationView', 'MediaDisplay', 'TaskStore', 'WorkflowPlan')][string]$Suite = 'All'
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$cargo = (Resolve-Path -LiteralPath (Join-Path $RustBin 'cargo.exe')).Path
$rustc = (Resolve-Path -LiteralPath (Join-Path $RustBin 'rustc.exe')).Path
$cl = (Get-Command cl.exe -CommandType Application | Select-Object -First 1).Source
$link = (Get-Command link.exe -CommandType Application | Select-Object -First 1).Source
if ($env:VSCMD_ARG_TGT_ARCH -ne 'x64' -or -not $env:VCToolsInstallDir -or
    -not $cl.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase) -or
    -not $link.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Initialize x64 VS Developer PowerShell before running this check.'
}

if (-not $OfflineCargoHome) { $OfflineCargoHome = Join-Path $repo 'target\dev\cargo-home' }
$cache = (Resolve-Path -LiteralPath $OfflineCargoHome).Path
foreach ($variable in Get-ChildItem Env:) {
    if ($variable.Name -match '^(CARGO($|_)|RUST|VCPKG|PKG_CONFIG|BINDGEN|CL$|_CL_$|LINK$|_LINK_$|((HOST|TARGET)_)?(CC|CXX|AR|CFLAGS|CPPFLAGS|CXXFLAGS|ARFLAGS)($|_))|SQLITE|SQLCIPHER') {
        [Environment]::SetEnvironmentVariable($variable.Name, $null, 'Process')
    }
}
$out = Join-Path $repo 'target\p01-04'
$run = [Guid]::NewGuid().ToString('N')
$env:RUSTC = $rustc
$env:CC = $cl
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $link
$env:CARGO_HOME = $cache
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = 'sparse'
$env:CARGO_TERM_COLOR = 'never'
$env:CARGO_TARGET_DIR = Join-Path $out 'cargo-target'
$env:TEMP = Join-Path $out 'temp'
$env:TMP = $env:TEMP
$logs = Join-Path $out 'logs'
$directories = @($env:TEMP, $logs)
if ($Suite -in @('All', 'Connection')) {
    $env:DD_SQLITE_INIT_RUN_DIR = Join-Path $repo "target\p01-03\runs\$run"
    $directories += $env:DD_SQLITE_INIT_RUN_DIR
}
if ($Suite -in @('All', 'Migration', 'AppMigration', 'WorkflowPlan')) {
    $env:DD_SQLITE_MIGRATION_RUN_DIR = Join-Path $out "runs\$run"
    $directories += $env:DD_SQLITE_MIGRATION_RUN_DIR
}
if ($Suite -in @('All', 'WorkspaceLock', 'WorkspaceOpen', 'WorkspaceAccess', 'WorkspaceThread', 'WorkspaceCreate', 'ImportAuthorization', 'ImportPlan', 'DatasetBrowse', 'ReviewSession', 'AnnotationView', 'MediaDisplay', 'TaskStore')) {
    $env:DD_WORKSPACE_LOCK_RUN_DIR = Join-Path $repo "target\p02-workspace\runs\$run"
    $directories += $env:DD_WORKSPACE_LOCK_RUN_DIR
}
if ($Suite -in @('All', 'ImportPreflight', 'ImportRegistration')) {
    $env:DD_IMPORT_PREFLIGHT_RUN_DIR = Join-Path $repo "target\import-preflight\runs\$run"
    $directories += $env:DD_IMPORT_PREFLIGHT_RUN_DIR
}
New-Item -ItemType Directory -Path $directories -Force | Out-Null
if ($Suite -in @('All', 'ImportPreflight')) {
    $escape = Join-Path $env:DD_IMPORT_PREFLIGHT_RUN_DIR 'junction-source'
    $outside = Join-Path $env:DD_IMPORT_PREFLIGHT_RUN_DIR 'junction-target'
    New-Item -ItemType Directory -Path $escape, $outside | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $escape 'redirected') -Target $outside | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'testdata\image_io\p00-worker-rgb.png') -Destination (Join-Path $outside 'unapproved.png')
}
$log = Join-Path $logs "$run.log"
$cargoArgs = @('test', '--manifest-path', (Join-Path $repo 'Cargo.toml'), '--locked', '--offline',
    '--target', 'x86_64-pc-windows-msvc')
switch ($Suite) {
    'Connection' { $cargoArgs += @('-p', 'dd-sqlite-connection-init-tests', '--test', 'workspace_connection') }
    'Migration' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_migration') }
    'AppMigration' { $cargoArgs += @('-p', 'dd-infra', '--test', 'app_migration') }
    'WorkspaceBackup' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_backup') }
    'BackupAssets' { $cargoArgs += @('-p', 'dd-infra', '--test', 'backup_assets') }
    'BackupArtifacts' { $cargoArgs += @('-p', 'dd-infra', '--test', 'backup_artifacts') }
    'WorkspaceLock' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_ownership') }
    'WorkspaceOpen' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_open') }
    'WorkspaceAccess' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_access') }
    'VideoFileGrants' { $cargoArgs += @('-p', 'dd-infra', '--test', 'video_file_grants') }
    'VideoRegistration' { $cargoArgs += @('-p', 'dd-infra', '--test', 'video_registration') }
    'WorkspaceThread' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_thread') }
    'WorkspaceCreate' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workspace_create') }
    'ImportPreflight' { $cargoArgs += @('-p', 'dd-infra', '--test', 'import_preflight') }
    'ImportRegistration' { $cargoArgs += @('-p', 'dd-infra', '--test', 'import_registration') }
    'ImportAuthorization' { $cargoArgs += @('-p', 'dd-infra', '--test', 'import_authorization') }
    'ImportPlan' { $cargoArgs += @('-p', 'dd-infra', '--test', 'import_plan') }
    'DatasetBrowse' { $cargoArgs += @('-p', 'dd-infra', '--test', 'dataset_browse') }
    'InputFingerprint' { $cargoArgs += @('-p', 'dd-infra', '--test', 'input_fingerprint') }
    'ReviewValidation' { $cargoArgs += @('-p', 'dd-infra', '--test', 'review_validation') }
    'ReviewSession' { $cargoArgs += @('-p', 'dd-infra', '--test', 'review_session') }
    'MediaDisplay' { $cargoArgs += @('-p', 'dd-infra', '--test', 'media_display') }
    'AnnotationView' { $cargoArgs += @('-p', 'dd-infra', '--test', 'annotation_view') }
    'TaskStore' { $cargoArgs += @('-p', 'dd-infra', '--test', 'task_store') }
    'WorkflowPlan' { $cargoArgs += @('-p', 'dd-infra', '--test', 'workflow_plan') }
    'All' { $cargoArgs += @('-p', 'dd-infra', '-p', 'dd-sqlite-connection-init-tests', '--all-targets') }
}
$cargoArgs += @('--', '--nocapture')

Push-Location -LiteralPath $repo
try {
    "MSVC=$env:VCToolsVersion suite=$Suite log=$log" | Tee-Object -FilePath $log
    "> $cargo $cargoArgs" | Tee-Object -FilePath $log -Append
    # Keep native stderr in the test log and use Cargo's exit status.
    $ErrorActionPreference = 'Continue'
    $PSNativeCommandUseErrorActionPreference = $false
    & $cargo @cargoArgs 2>&1 | ForEach-Object { $_.ToString() } | Tee-Object -FilePath $log -Append
    $result = $LASTEXITCODE
    "exit_code=$result" | Tee-Object -FilePath $log -Append
} finally {
    Pop-Location
}
exit $result
