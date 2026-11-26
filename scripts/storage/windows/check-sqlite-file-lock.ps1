[CmdletBinding()]
param(
    [string]$CargoExe,
    [string]$RustcExe,
    [string]$OfflineCargoHome
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$target = Join-Path $repo 'target'
$output = Join-Path $target 'p00-03'
$logs = Join-Path $output 'logs'
$runs = Join-Path $output 'runs'
$build = Join-Path $output 'cargo-target'
$temp = Join-Path $output 'temp'
$manifest = Join-Path $repo 'tests\storage\sqlite\file-lock\Cargo.toml'
$lock = Join-Path ([IO.Path]::GetDirectoryName($manifest)) 'Cargo.lock'
$triple = 'x86_64-pc-windows-msvc'
$utf8 = [Text.UTF8Encoding]::new($false)
$lockHash = '959D17B6FA2551761C813CA26D1CD31F4A4676C214ACC596B39D677D93953647'
$sourceId = '2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76'

function Initialize-Directory([string]$Path) {
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

function Get-ToolPath([string]$Path, [string]$Label) {
    if ($Path -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is unavailable. Pass its absolute installed path: $Path"
    }
    $item = Get-Item -LiteralPath $Path
    if (($item.Attributes -bor $item.Directory.Attributes) -band [IO.FileAttributes]::ReparsePoint) {
        throw "Use the physical $Label executable, outside tool aliases."
    }
    return $item.FullName
}

# A private Windows job owns each command and its descendants. The broker starts
# the command only after assignment succeeds. Closing the handle cancels the job.
$broker = @'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if ([Console]::ReadLine() -cne 'run') { exit 1 }
$program = $env:DD_P00_PROGRAM
$arguments = @(ConvertFrom-Json -InputObject $env:DD_P00_ARGUMENTS)
$env:DD_P00_PROGRAM = $null
$env:DD_P00_ARGUMENTS = $null
$global:LASTEXITCODE = $null
& $program @arguments 2>&1 | ForEach-Object {
    if ($_ -is [Management.Automation.ErrorRecord]) { [Console]::WriteLine($_.Exception.Message) }
    else { [Console]::WriteLine($_.ToString()) }
}
if ($null -eq $global:LASTEXITCODE) { exit 127 }
exit $global:LASTEXITCODE
'@
$encodedBroker = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($broker))

function Initialize-JobApi {
    # Add-Type may use temporary files on Windows PowerShell 5.1.
    $savedTmp = $env:TMP
    $savedTemp = $env:TEMP
    try {
        $env:TMP = $temp
        $env:TEMP = $temp
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class SqliteSmokeJob {
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr CreateJobObjectW(IntPtr attributes, IntPtr name);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(IntPtr job, int type, byte[] info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr job);

    public static IntPtr Open() {
        IntPtr job = CreateJobObjectW(IntPtr.Zero, IntPtr.Zero);
        if (job == IntPtr.Zero) throw new Win32Exception();
        // x64 JOBOBJECT_EXTENDED_LIMIT_INFORMATION, LimitFlags at offset 16.
        byte[] limits = new byte[144];
        BitConverter.GetBytes((uint)0x2000).CopyTo(limits, 16); // KILL_ON_JOB_CLOSE
        if (SetInformationJobObject(job, 9, limits, (uint)limits.Length)) return job;
        var error = new Win32Exception();
        CloseHandle(job);
        throw error;
    }
}
'@
    } finally {
        $env:TMP = $savedTmp
        $env:TEMP = $savedTemp
    }
}

function Invoke-CheckCommand([string]$File, [string[]]$Arguments, [int]$Timeout = 30000) {
    Write-CheckLog ('> ' + $File + ' ' + (($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '))
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\v1.0\powershell.exe'
    $start.Arguments = "-NoProfile -NonInteractive -EncodedCommand $encodedBroker"
    $start.WorkingDirectory = $repo
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $utf8
    $start.StandardErrorEncoding = $utf8
    foreach ($name in @($start.EnvironmentVariables.Keys)) {
        if ($name -match '^(CARGO($|_)|RUST|VCPKG|PKG_CONFIG|BINDGEN|CL$|_CL_$|LINK$|_LINK_$|((HOST|TARGET)_)?(CC|CXX|AR|CFLAGS|CPPFLAGS|CXXFLAGS|ARFLAGS)($|_))|SQLITE|SQLCIPHER') {
            $start.EnvironmentVariables.Remove($name)
        }
    }
    foreach ($name in $childEnvironment.Keys) {
        $start.EnvironmentVariables[$name] = $childEnvironment[$name]
    }
    $start.EnvironmentVariables['DD_P00_PROGRAM'] = $File
    $start.EnvironmentVariables['DD_P00_ARGUMENTS'] = ConvertTo-Json -InputObject $Arguments -Compress
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $job = [IntPtr]::Zero
    $started = $false
    try {
        $job = [SqliteSmokeJob]::Open()
        $started = $process.Start()
        if (-not $started -or -not [SqliteSmokeJob]::AssignProcessToJobObject($job, $process.Handle)) {
            throw 'Could not contain the command in a private Windows process job.'
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.WriteLine('run')
        $process.StandardInput.Close()
        $finished = $process.WaitForExit($Timeout)
        [void][SqliteSmokeJob]::CloseHandle($job)
        $job = [IntPtr]::Zero
        if ($stdout.Wait(3000) -and $stdout.Result) { Write-CheckLog $stdout.Result.TrimEnd() }
        if ($stderr.Wait(3000) -and $stderr.Result) { Write-CheckLog $stderr.Result.TrimEnd() }
        if (-not $finished) { throw "Command timed out after $Timeout ms: $File" }
        if (-not $stdout.IsCompleted -or -not $stderr.IsCompleted) { throw "Command output did not close: $File" }
        Write-CheckLog "exit_code=$($process.ExitCode)"
        return @{ Code = $process.ExitCode; Text = $stdout.Result + $stderr.Result }
    } finally {
        if ($job -ne [IntPtr]::Zero) { [void][SqliteSmokeJob]::CloseHandle($job) }
        if ($started) {
            try { $process.StandardInput.Close() } catch {}
            [void]$process.WaitForExit(3000)
        }
        $process.Dispose()
    }
}

function Assert-Success($Result, [string]$Label) {
    if ($Result.Code -ne 0) { throw "$Label failed with exit $($Result.Code). See $log" }
}

function Get-CachedFile([string]$RegistryPath, [string]$Relative) {
    $matches = @(Get-ChildItem -LiteralPath $RegistryPath -Directory | ForEach-Object {
        $candidate = Join-Path $_.FullName $Relative
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $candidate }
    })
    if ($matches.Count -ne 1) { throw "Expected one approved cache entry for $Relative" }
    return $matches[0]
}


try {
    foreach ($directory in @($target, $output, $logs, $runs, $build, $temp)) { Initialize-Directory $directory }
    $log = Join-Path $logs 'check.log'
    $existing = Get-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue
    if ($existing -and ($existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain check log: $log"
    }
    [IO.File]::WriteAllText($log, '', $utf8)
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}

$failureExit = 2
try {
    $disk = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($output))
    if (-not [Environment]::Is64BitProcess -or $disk.DriveType -ne 'Fixed' -or $disk.DriveFormat -ne 'NTFS') {
        throw 'This check requires 64-bit Windows PowerShell and a local fixed NTFS volume.'
    }
    Write-CheckLog "OS=$([Environment]::OSVersion.VersionString) CPU=$env:PROCESSOR_ARCHITECTURE drive=$($disk.Name) type=$($disk.DriveType) filesystem=$($disk.DriveFormat)"
    $cargo = Get-ToolPath $CargoExe 'Cargo'
    $rustc = Get-ToolPath $RustcExe 'rustc'
    if ($OfflineCargoHome -notmatch '^[A-Za-z]:[\\/]' -or -not (Test-Path -LiteralPath $OfflineCargoHome -PathType Container)) {
        throw 'The approved offline Cargo home is unavailable.'
    }
    $cache = (Get-Item -LiteralPath $OfflineCargoHome).FullName
    if ($cache -ine (Join-Path $output 'cargo-home')) { throw "Use the dedicated Cargo home under $output." }
    if ((Get-Item -LiteralPath $cache -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'The dedicated Cargo home must be a plain directory.'
    }
    $cl = (Get-Command cl.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $link = (Get-Command link.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    if (-not $env:VCToolsInstallDir -or $env:VSCMD_ARG_TGT_ARCH -ne 'x64' -or
        -not $cl.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase) -or
        -not $link.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Initialize the installed x64 VS Developer PowerShell with MSVC cl.exe and link.exe.'
    }
    Write-CheckLog "MSVC=$env:VCToolsVersion cl=$cl link=$link"
    $configDirs = @(Join-Path $cache '.')
    for ($ancestor = [IO.DirectoryInfo]::new($repo); $ancestor; $ancestor = $ancestor.Parent) {
        $configDirs += Join-Path $ancestor.FullName '.cargo'
    }
    foreach ($directory in $configDirs) {
        foreach ($name in @('config','config.toml')) {
            $config = Join-Path $directory $name
            if (Test-Path -LiteralPath $config) { throw "Unexpected Cargo config could override the bundled build: $config" }
        }
    }
    if (-not (Test-Path -LiteralPath $lock -PathType Leaf)) { throw "Approved Cargo.lock is unavailable: $lock" }
    if ((Get-FileHash -LiteralPath $lock -Algorithm SHA256).Hash -cne $lockHash) {
        $failureExit = 1
        throw 'Cargo.lock differs from the approved pinned dependency set.'
    }
    Write-CheckLog "Cargo.lock SHA-256=$lockHash"
    $run = Join-Path $runs ([guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($run)
    Write-CheckLog "synthetic_run=$run"
    $sdkTools = if ($env:WindowsSdkVerBinPath) { Join-Path $env:WindowsSdkVerBinPath 'x64' }
    $childPath = @(
        [IO.Path]::GetDirectoryName($cl)
        [IO.Path]::GetDirectoryName($rustc)
        $sdkTools
        [Environment]::SystemDirectory
        [Environment]::GetFolderPath('Windows')
    ) | Where-Object { $_ } | Select-Object -Unique
    $childEnvironment = @{
        PATH = $childPath -join ';'
        CARGO_HOME = $cache
        CARGO_TARGET_DIR = $build
        CARGO_NET_OFFLINE = 'true'
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL = 'sparse'
        CARGO_TERM_COLOR = 'never'
        CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $link
        RUSTC = $rustc
        CC = $cl
        TEMP = $temp
        TMP = $temp
        DD_SQLITE_SMOKE_DIR = $run
    }
    Initialize-JobApi
    $cargoVersion = Invoke-CheckCommand $cargo @('--version')
    Assert-Success $cargoVersion 'Cargo version probe'
    if ($cargoVersion.Text -notmatch '(?m)^cargo 1\.97\.1(?:\s|$)') { throw 'Expected Cargo 1.97.1.' }
    $rustVersion = Invoke-CheckCommand $rustc @('-Vv')
    Assert-Success $rustVersion 'rustc version probe'
    if ($rustVersion.Text -notmatch '(?m)^release: 1\.97\.1\s*$' -or $rustVersion.Text -notmatch "(?m)^host: $triple\s*$") {
        throw "Expected Rust 1.97.1 for $triple."
    }
    $args = @('--manifest-path', $manifest, '--locked', '--offline', '--target', $triple)
    Assert-Success (Invoke-CheckCommand $cargo (@('fetch') + $args) 60000) 'Offline dependency fetch'
    $features = Invoke-CheckCommand $cargo (@('tree') + $args + @('-e','features')) 60000
    Assert-Success $features 'Offline feature resolution'
    $archive = Get-CachedFile (Join-Path $cache 'registry\cache') 'libsqlite3-sys-0.40.0.crate'
    $source = Get-CachedFile (Join-Path $cache 'registry\src') 'libsqlite3-sys-0.40.0\sqlite3\sqlite3.c'

    $failureExit = 1
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    $sourceHash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
    if ($archiveHash -cne 'B06E69070B7E12262B0CD90832F0A7396F142A2B330181255562DECF9F0C6215' -or
        $sourceHash -cne '0C657666D819F261CC4007526AD57E1669DE9949DF7A8EA847AA284A2BF0EE52') {
        throw 'The bundled archive or amalgamation differs from the approved source.'
    }
    Write-CheckLog "libsqlite3-sys archive SHA-256=$archiveHash"
    Write-CheckLog "sqlite3.c SHA-256=$sourceHash"
    $markers = (Select-String -LiteralPath $source -Pattern '^#define SQLITE_(VERSION|SOURCE_ID)\s+' | ForEach-Object Line) -join [Environment]::NewLine
    if ($markers -notmatch '(?m)^#define SQLITE_VERSION\s+"3\.54\.1"' -or -not $markers.Contains($sourceId)) {
        throw 'The bundled amalgamation version or Source ID does not match the handoff.'
    }
    Write-CheckLog $markers
    if ($features.Text -notmatch 'rusqlite feature "bundled"' -or $features.Text -notmatch 'rusqlite feature "backup"' -or
        $features.Text -notmatch 'libsqlite3-sys feature "bundled"' -or $features.Text -match 'feature "[^"]*sqlcipher') {
        throw 'The dependency feature graph does not match the approved bundled and backup build.'
    }
    $test = Invoke-CheckCommand $cargo (@('test') + $args + @('-vv','--','--nocapture')) 300000
    Assert-Success $test 'Offline build and file-lock test'
    if ($test.Text -notmatch '(?m)^sqlite_version=3\.54\.1\s*$' -or
        -not $test.Text.Contains("sqlite_source_id=$sourceId") -or
        $test.Text -notmatch 'contender SQLITE_BUSY primary=5' -or
        $test.Text -notmatch 'reopened synthetic database contains baseline, contender, holder') {
        throw 'The actual SQLite version or file-lock runtime evidence is incomplete.'
    }

    $debug = Join-Path $build "$triple\debug"
    $nativeBuild = Join-Path $debug 'build'
    $static = @(Get-ChildItem -LiteralPath $nativeBuild -Directory -Filter 'libsqlite3-sys-*' | ForEach-Object {
        $record = Join-Path $_.FullName 'output'
        $native = Join-Path $_.FullName 'out'
        if ((Test-Path -LiteralPath $record -PathType Leaf) -and
            (Select-String -LiteralPath $record -Pattern '^cargo:rustc-link-lib=static=sqlite3\s*$' -Quiet)) {
            Get-ChildItem -LiteralPath $native -File | Where-Object Name -In @('sqlite3.lib','libsqlite3.a')
        }
    })
    if (-not $static) { throw 'Missing Cargo static sqlite3 link directive or built native archive.' }
    foreach ($library in $static) { Write-CheckLog "cargo:rustc-link-lib=static=sqlite3 archive=$($library.FullName)" }
    $imports = Invoke-CheckCommand $link @('/DUMP','/DEPENDENTS',(Join-Path $debug 'sqlite_lock_worker.exe'))
    Assert-Success $imports 'MSVC import inspection'
    if ($imports.Text -match '(?im)^\s*[^\s]*(sqlite|sqlcipher)[^\s]*\.dll\s*$') { throw 'Worker imports a SQLite or SQLCipher DLL.' }
    if ((Get-FileHash -LiteralPath $lock -Algorithm SHA256).Hash -cne $lockHash) { throw 'The dependency lock changed during the run.' }
    Write-CheckLog "[result] PASS: bundled SQLite, NTFS WAL and two-process lock smoke"
    exit 0
} catch {
    Write-CheckLog "[result] exit=$failureExit $($_.Exception.Message)"
    exit $failureExit
}
