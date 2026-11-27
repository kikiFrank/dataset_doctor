[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('rust','typescript','python','contracts','sqlite','integration','desktop')][string]$Check,
    [string]$RustBin, [string]$PythonExe, [string]$UvExe, [string]$PnpmExe,
    [string]$ProtocExe, [string]$GoogleProtoRoot, [string]$FfmpegExe, [string]$FfprobeExe,
    [string]$SchemaCache,
    [switch]$AllowNetwork
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Set-Location -LiteralPath $repo
$dev = Join-Path $repo 'target/dev'
$tools = Join-Path $dev 'ci-tools'
$output = Join-Path $repo "target/ci/$Check"
New-Item -ItemType Directory -Force -Path $tools,$output | Out-Null
$lock = Get-Content config/toolchains/windows-x64.json -Raw | ConvertFrom-Json
$archives = Get-Content config/toolchains/ci-windows-x64.json -Raw | ConvertFrom-Json
$rust = $Check -notin @('typescript','python')
$worker = $Check -in @('python','contracts','integration','desktop')
$nodePackages = $Check -in @('typescript','desktop')
$video = $Check -in @('python','integration')

function Run([string]$Exe, [string[]]$Arguments) {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "CI prerequisite exited with code $LASTEXITCODE" }
}
function Tool([string]$Explicit, [string]$Name) {
    if ($Explicit) { return (Resolve-Path -LiteralPath $Explicit).Path }
    return (Get-Command $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
function Version([string]$Exe, [string[]]$Arguments, [string]$Expected) {
    $actual = (& $Exe @Arguments | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $actual -cnotmatch ('^' + [regex]::Escape($Expected) + '(?: \([^\r\n]*\))?$')) { throw "Locked version mismatch. Expected '$Expected', observed '$actual'." }
}
function Archive($Spec, [string]$Name) {
    $archive = Join-Path $tools ([IO.Path]::GetFileName(([uri]$Spec.url).AbsolutePath))
    if (-not (Test-Path -LiteralPath $archive)) {
        if (-not $AllowNetwork) { throw "Offline tool archive missing: $archive" }
        Invoke-WebRequest -Uri $Spec.url -OutFile $archive
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $Spec.sha256) {
        throw "Tool archive digest mismatch: $Name"
    }
    # Expand only after digest verification. Existing cache contents are checked
    # against this verified archive on each preparation.
    Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $tools $Name) -Force
}

$environment = [ordered]@{
    check=$Check; repo=$repo; toolchain=$lock
    os_version=[Environment]::OSVersion.VersionString
    architecture=$env:PROCESSOR_ARCHITECTURE
}
if ($rust) {
    if (-not $RustBin) {
        if ($AllowNetwork) { Run 'rustup' @('toolchain','install',$lock.rustc,'--profile','minimal','--component','clippy,rustfmt') }
        $compiler = & rustup which --toolchain $lock.rustc rustc
        if ($LASTEXITCODE -ne 0) { throw 'Locked Rust toolchain is unavailable.' }
        $RustBin = Split-Path -Parent $compiler
    }
    $RustBin = (Resolve-Path -LiteralPath $RustBin).Path
    $env:PATH = $RustBin + [IO.Path]::PathSeparator + $env:PATH
    $env:RUSTC = Join-Path $RustBin 'rustc.exe'
    $cargo = Join-Path $RustBin 'cargo.exe'
    $actual = & $env:RUSTC --version
    if ($LASTEXITCODE -ne 0 -or $actual -notmatch ('^rustc ' + [regex]::Escape($lock.rustc) + ' ')) { throw 'Locked Rust version mismatch.' }
    $environment.rustc_version = $actual
    $cargoVersion = & $cargo --version
    if ($LASTEXITCODE -ne 0 -or $cargoVersion -notmatch ('^cargo ' + [regex]::Escape($lock.cargo) + ' ')) { throw 'Locked Cargo version mismatch.' }
    $environment.cargo_version = $cargoVersion
    $env:CARGO_HOME = Join-Path $dev 'cargo-home'
    $env:CARGO_TARGET_DIR = Join-Path $dev 'cargo-target'
    New-Item -ItemType Directory -Path $env:CARGO_HOME -Force | Out-Null
    $environment.rust_bin = $RustBin
    if (-not $ProtocExe) { Archive $archives.protoc 'protoc-29.3'; $ProtocExe = Join-Path $tools "$($archives.protoc.bin_directory)/protoc.exe" }
    $ProtocExe = Tool $ProtocExe 'protoc.exe'
    Version $ProtocExe @('--version') "libprotoc $($lock.protoc)"
    if (-not $GoogleProtoRoot) { $GoogleProtoRoot = Join-Path (Split-Path -Parent $ProtocExe) '../include' }
    $environment.protoc = $ProtocExe
    $environment.google_proto_root = (Resolve-Path -LiteralPath $GoogleProtoRoot).Path
    $fetch = @('fetch','--locked','--target','x86_64-pc-windows-msvc')
    if (-not $AllowNetwork) { $fetch += '--offline' }
    Run $cargo $fetch
    if ($Check -eq 'sqlite') {
        $workspaceCargoHome = $env:CARGO_HOME
        $env:CARGO_HOME = Join-Path $repo 'target/p00-03/cargo-home'
        New-Item -ItemType Directory -Force -Path $env:CARGO_HOME | Out-Null
        try { Run $cargo ($fetch + @('--manifest-path','tests/storage/sqlite/file-lock/Cargo.toml')) }
        finally { $env:CARGO_HOME = $workspaceCargoHome }
    }
}
if ($worker) {
    $PythonExe = Tool $PythonExe 'python.exe'
    $UvExe = Tool $UvExe 'uv.exe'
    Version $PythonExe @('-I','-B','-c','import sys; print("cpython " + sys.version.split()[0])') $lock.python
    Version $UvExe @('--version') "uv $($lock.uv)"
    $environment.python = $PythonExe
    $environment.uv = $UvExe
    $env:UV_PROJECT_ENVIRONMENT = Join-Path $dev 'worker-venv'
    $sync = @('sync','--locked','--no-build','--no-python-downloads','--no-config','--project','services/python-worker','--cache-dir',(Join-Path $dev 'uv-cache'),'--python',$PythonExe)
    if (-not $AllowNetwork) { $sync += '--offline' }
    Run $UvExe $sync
}
if ($nodePackages -or $Check -eq 'contracts') {
    $node = Tool '' 'node.exe'
    Version $node @('--version') "v$($lock.node)"
    $environment.node = $node
}
if ($nodePackages) {
    $PnpmExe = Tool $PnpmExe 'pnpm.exe'
    Version $PnpmExe @('--version') $lock.pnpm
    $environment.pnpm = $PnpmExe
    $install = @('--dir','apps/desktop','install','--frozen-lockfile','--ignore-scripts','--store-dir',(Join-Path $dev 'pnpm-store'))
    if (-not $AllowNetwork) { $install += '--offline' }
    Run $PnpmExe $install
}
if ($video) {
    if (-not $FfmpegExe -or -not $FfprobeExe) {
        Archive $archives.ffmpeg 'ffmpeg'
        $FfmpegExe = Join-Path $tools "$($archives.ffmpeg.bin_directory)/ffmpeg.exe"
        $FfprobeExe = Join-Path $tools "$($archives.ffmpeg.bin_directory)/ffprobe.exe"
    }
    $environment.ffmpeg = (Resolve-Path -LiteralPath $FfmpegExe).Path
    $environment.ffprobe = (Resolve-Path -LiteralPath $FfprobeExe).Path
}
if ($Check -eq 'contracts') {
    if (-not $SchemaCache) { $SchemaCache = Join-Path $repo 'dist/p01-01/uv-cache' }
    New-Item -ItemType Directory -Path $SchemaCache -Force | Out-Null
    $environment.schema_cache = (Resolve-Path -LiteralPath $SchemaCache).Path
    $env:UV_PROJECT_ENVIRONMENT = Join-Path $repo 'dist/p01-01/venv'
    $sync = @('sync','--locked','--no-build','--no-python-downloads','--no-config','--prerelease','disallow','--keyring-provider','disabled','--default-index','https://pypi.org/simple','--project','tests/contracts/json-schema','--cache-dir',$SchemaCache,'--python',$PythonExe)
    if (-not $AllowNetwork) { $sync += '--offline' }
    Run $UvExe $sync
}
$environment | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'environment.json') -Encoding utf8
Write-Output "Prepared $Check prerequisites. Acceptance checks have not run."
