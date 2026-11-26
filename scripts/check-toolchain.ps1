# P00-01: inspect the existing Windows toolchain without installing anything.
[CmdletBinding()]
param([switch]$Locked)

$tools = @(
    @{ Name = 'cargo'; Args = '--version'; Pattern = '^cargo (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the bin directory of an already installed Rust toolchain.' }
    @{ Name = 'rustc'; Args = '--version'; Pattern = '^rustc (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the bin directory of an already installed Rust toolchain.' }
    @{ Name = 'node'; Args = '--version'; Pattern = '^v(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$'; Target = '^24\.\d+\.\d+$'; Hint = 'Expose a local Node 24 release executable.' }
    @{ Name = 'pnpm'; Args = '--version'; Pattern = '^(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$'; Hint = 'Expose a locally installed standalone pnpm.exe.' }
    @{ Name = 'python'; Args = '-E -S -B -c "import sys; print(''%s %s'' % (getattr(getattr(sys, ''implementation'', None), ''name'', ''python''), sys.version.split()[0]))"'; Pattern = '^(?<version>[a-z]+ \d+\.\d+\.\d+[a-z0-9]*)$'; Target = '^cpython 3\.12\.\d+$'; Hint = 'Expose a local CPython 3.12 release executable.' }
    @{ Name = 'uv'; Args = '--version'; Pattern = '^uv (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the local uv executable.' }
    @{ Name = 'protoc'; Args = '--version'; Pattern = '^libprotoc (?<version>\d+(?:\.\d+){1,2})(?:\s|$)'; Hint = 'Expose the local protoc executable.' }
)

$pinned = $null
if ($Locked) {
    if (-not [Environment]::Is64BitProcess -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        Write-Output 'The locked toolchain requires a Windows x64 process.'
        exit 1
    }
    try {
        $lockFile = Join-Path $PSScriptRoot '..\config\toolchains\windows-x64.json'
        $pinned = Get-Content -LiteralPath $lockFile -Raw -Encoding UTF8 -ErrorAction Stop |
            ConvertFrom-Json -ErrorAction Stop
        $missing = @($tools | Where-Object { $pinned.($_.Name) -isnot [string] -or -not $pinned.($_.Name) })
        if ($missing -or @($pinned.PSObject.Properties).Count -ne $tools.Count) {
            throw 'Every probe must have one exact version.'
        }
    } catch {
        Write-Output "Invalid Windows toolchain lock: $($_.Exception.Message)"
        exit 1
    }
    Write-Output 'Locked toolchain: windows-x64'
}

# Use in-memory interop so the preflight does not compile or write a helper file.
function New-WindowsApi {
    $name = [Reflection.AssemblyName]::new('DatasetDoctorToolchainCheck')
    $access = [Reflection.Emit.AssemblyBuilderAccess]::Run
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        $assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly($name, $access)
    } else {
        $assembly = [Reflection.Emit.AssemblyBuilder]::DefineDynamicAssembly($name, $access)
    }
    $module = $assembly.DefineDynamicModule($name.Name)
    $class = $module.DefineType('ToolchainWindowsApi', [Reflection.TypeAttributes]'Public, Sealed')
    $methods = @(
        @{ Name = 'QueryDosDeviceW'; Returns = [uint32]; Parameters = @([string], [Text.StringBuilder], [uint32]) }
        @{ Name = 'CreateFileW'; Returns = [IntPtr]; Parameters = @([string], [uint32], [uint32], [IntPtr], [uint32], [uint32], [IntPtr]) }
        @{ Name = 'GetFileInformationByHandleEx'; Returns = [bool]; Parameters = @([IntPtr], [int], [byte[]], [uint32]) }
        @{ Name = 'CreateJobObjectW'; Returns = [IntPtr]; Parameters = @([IntPtr], [IntPtr]) }
        @{ Name = 'SetInformationJobObject'; Returns = [bool]; Parameters = @([IntPtr], [int], [byte[]], [uint32]) }
        @{ Name = 'AssignProcessToJobObject'; Returns = [bool]; Parameters = @([IntPtr], [IntPtr]) }
        @{ Name = 'CloseHandle'; Returns = [bool]; Parameters = @([IntPtr]) }
    )
    foreach ($entry in $methods) {
        $method = $class.DefinePInvokeMethod(
            $entry.Name, 'kernel32.dll', [Reflection.MethodAttributes]'Public, Static, PinvokeImpl',
            [Reflection.CallingConventions]::Standard, $entry.Returns, [type[]]$entry.Parameters,
            [Runtime.InteropServices.CallingConvention]::Winapi, [Runtime.InteropServices.CharSet]::Unicode
        )
        $method.SetImplementationFlags($method.GetMethodImplementationFlags() -bor [Reflection.MethodImplAttributes]::PreserveSig)
    }
    return $class.CreateType()
}

function Close-PathHandles($handles) {
    foreach ($handle in $handles) { [void]$windowsApi::CloseHandle($handle) }
}

function Open-PlainPathItem($path, [bool]$directory) {
    # FILE_READ_DATA/LIST_DIRECTORY | FILE_READ_ATTRIBUTES, FILE_SHARE_READ, OPEN_EXISTING, OPEN_REPARSE_POINT | BACKUP_SEMANTICS.
    $handle = $windowsApi::CreateFileW($path, 0x81, 1, [IntPtr]::Zero, 3, 0x02200000, [IntPtr]::Zero)
    if ($handle -eq [IntPtr](-1)) { return @{ Reason = 'Windows could not open the path for verification' } }

    $attributesAndTag = [byte[]]::new(8)
    if ($windowsApi::GetFileInformationByHandleEx($handle, 9, $attributesAndTag, $attributesAndTag.Length)) {
        $attributes = [BitConverter]::ToUInt32($attributesAndTag, 0)
        if ($attributes -band [IO.FileAttributes]::ReparsePoint) {
            $reason = 'a reparse point was found'
        } elseif ([bool]($attributes -band [IO.FileAttributes]::Directory) -eq $directory) {
            return @{ Handle = $handle }
        } else {
            $reason = 'the path has an unexpected file type'
        }
    } else {
        $reason = 'Windows could not verify the path attributes'
    }
    Close-PathHandles $handle
    return @{ Reason = $reason }
}

function Open-PlainDirectory($path) {
    $root = [IO.Path]::GetPathRoot($path)
    $device = [Text.StringBuilder]::new(2048)
    # Reject mapped drives and substituted drive letters as well as UNC paths.
    $deviceLength = $windowsApi::QueryDosDeviceW($root.Substring(0, 2), $device, $device.Capacity)
    if (-not $deviceLength -or $device.ToString() -notmatch '^\\Device\\HarddiskVolume[0-9]+$') { return }

    $handles = [Collections.Generic.List[IntPtr]]::new()
    $current = $root
    foreach ($component in @('') + $path.Substring($root.Length).Split([char]'\')) {
        if ($component) { $current = [IO.Path]::Combine($current, $component) }
        if (-not $component -and $handles.Count) { continue }
        $item = Open-PlainPathItem $current $true
        if ($item.Reason) {
            Close-PathHandles $handles
            return @{ Reason = $item.Reason; BlockedPath = $current }
        }
        $handles.Add($item.Handle)
    }
    return @{ Handles = $handles }
}

function Get-ToolEntry($directory, $name) {
    foreach ($extension in @('.exe', '.cmd', '.bat', '.ps1', '.com')) {
        $path = [IO.Path]::Combine($directory, "$name$extension")
        try {
            $attributes = [IO.File]::GetAttributes($path)
        } catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] {
            continue
        } catch {
            return @{ Path = $path; Reason = 'Windows could not verify the file entry' }
        }
        if ($attributes -band [IO.FileAttributes]::ReparsePoint) {
            return @{ Path = $path; Reason = 'an executable link or app alias was found' }
        }
        if ($attributes -band [IO.FileAttributes]::Directory) { continue }
        if ($extension -ne '.exe') { return @{ Path = $path; Reason = 'a command shim was found' } }
        return @{ Path = $path }
    }
}

function Get-ProxyReason($name, $path, $directory) {
    if ($path -match '[\\/](WindowsApps|corepack|shims|\.?volta|fnm_multishells)[\\/]|[\\/]Chocolatey[\\/]bin[\\/]|[\\/]WinGet[\\/]Links[\\/]') {
        return 'a tool manager or app alias path was found'
    }
    if ($name -in @('cargo', 'rustc')) {
        $rustup = Get-ToolEntry $directory 'rustup'
        if ($directory -ieq $cargoBin -or $directory -match '[\\/]\.cargo[\\/]bin$' -or $rustup) {
            return 'a possible Rustup proxy was found'
        }
    }

    $metadata = [Diagnostics.FileVersionInfo]::GetVersionInfo($path)
    $description = @($metadata.OriginalFilename, $metadata.ProductName, $metadata.FileDescription) -join ' '
    if ($description -match 'rustup|corepack|volta|shimgen|shim launcher|version manager|pyenv|fnm|mise\.exe') {
        return 'a tool manager or shim was found'
    }
}

# The broker starts only the requested executable, after the parent assigns it
# to a private, kill-on-close job. Closing stdin without a grant runs no tool.
$brokerSource = @'
$ErrorActionPreference = 'Stop'
try {
    if ([Console]::ReadLine() -cne 'probe') { exit 1 }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $env:DD_P00_PROBE_EXE
    $start.Arguments = $env:DD_P00_PROBE_ARGS
    $start.WorkingDirectory = [Environment]::SystemDirectory
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables.Remove('DD_P00_PROBE_EXE')
    $start.EnvironmentVariables.Remove('DD_P00_PROBE_ARGS')
    $toolProcess = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $toolProcess.StandardOutput.BaseStream.CopyToAsync([Console]::OpenStandardOutput())
        $stderr = $toolProcess.StandardError.BaseStream.CopyToAsync([IO.Stream]::Null)
        $toolProcess.WaitForExit()
        [void]$stdout.GetAwaiter().GetResult()
        [void]$stderr.GetAwaiter().GetResult()
        exit $toolProcess.ExitCode
    } finally {
        $toolProcess.Dispose()
    }
} catch {
    exit 1
}
'@

function Get-LocalVersion($tool, $path, $directory) {
    $systemDirectory = [Environment]::SystemDirectory
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = [IO.Path]::Combine($systemDirectory, 'WindowsPowerShell\v1.0\powershell.exe')
    $start.Arguments = "-NoProfile -NonInteractive -EncodedCommand $brokerCommand"
    $start.WorkingDirectory = $systemDirectory
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['DD_P00_PROBE_EXE'] = $path
    $start.EnvironmentVariables['DD_P00_PROBE_ARGS'] = $tool.Args
    $start.EnvironmentVariables['PATH'] = "$directory;$systemDirectory"

    foreach ($variable in @('NODE_OPTIONS', 'NODE_PATH', 'NODE_V8_COVERAGE', 'NODE_COMPILE_CACHE')) {
        $start.EnvironmentVariables.Remove($variable)
    }
    if ($tool.Name -eq 'pnpm') {
        $start.EnvironmentVariables.Remove('NPM_CONFIG_USE_NODE_VERSION')
        $start.EnvironmentVariables['COREPACK_ENABLE_NETWORK'] = '0'
        $start.EnvironmentVariables['COREPACK_ENABLE_AUTO_PIN'] = '0'
        $start.EnvironmentVariables['NPM_CONFIG_OFFLINE'] = 'true'
        $start.EnvironmentVariables['NPM_CONFIG_MANAGE_PACKAGE_MANAGER_VERSIONS'] = 'false'
        $start.EnvironmentVariables['NPM_CONFIG_USERCONFIG'] = 'NUL'
        $start.EnvironmentVariables['NPM_CONFIG_GLOBALCONFIG'] = 'NUL'
    }

    # JOBOBJECT_EXTENDED_LIMIT_INFORMATION. LimitFlags is at byte 16 on both
    # Windows architectures. KILL_ON_JOB_CLOSE assigns no breakaway permission.
    $limits = [byte[]]::new($(if ([IntPtr]::Size -eq 8) { 144 } else { 112 }))
    [BitConverter]::GetBytes([uint32]0x2000).CopyTo($limits, 16)
    $job = [IntPtr]::Zero
    $broker = [Diagnostics.Process]::new()
    $broker.StartInfo = $start
    $started = $false
    try {
        $job = $windowsApi::CreateJobObjectW([IntPtr]::Zero, [IntPtr]::Zero)
        if ($job -eq [IntPtr]::Zero -or -not $windowsApi::SetInformationJobObject($job, 9, $limits, $limits.Length)) {
            return @{ Reason = 'a safe process job could not be configured' }
        }
        $started = $broker.Start()
        if (-not $started -or -not $windowsApi::AssignProcessToJobObject($job, $broker.Handle)) {
            return @{ Reason = 'the probe could not be contained in a process job' }
        }

        $stdout = $broker.StandardOutput.ReadToEndAsync()
        $stderr = $broker.StandardError.ReadToEndAsync()
        $broker.StandardInput.WriteLine('probe')
        $broker.StandardInput.Close()
        if (-not $broker.WaitForExit(5000)) { return @{ Reason = 'the local version probe timed out' } }
        if ($broker.ExitCode -eq 0 -and $stdout.Wait(1000) -and $stderr.Wait(1000)) {
            $versionMatch = [regex]::Match($stdout.Result.Trim(), $tool.Pattern)
            if ($versionMatch.Success) { return @{ Version = $versionMatch.Groups['version'].Value } }
        }
    } catch {
        # Launch, containment, or output failures leave the version unverified.
    } finally {
        if ($job -ne [IntPtr]::Zero) { [void]$windowsApi::CloseHandle($job) }
        if ($started) {
            try { $broker.StandardInput.Close() } catch {}
            [void]$broker.WaitForExit(1000)
        }
        $broker.Dispose()
    }
    return @{ Reason = 'no version was confirmed by the local probe' }
}

try {
    $windowsApi = New-WindowsApi
    $brokerCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($brokerSource))
} catch {
    foreach ($tool in $tools) {
        Write-Output "$($tool.Name): UNVERIFIED. Windows path and process protection is unavailable. $($tool.Hint)"
    }
    exit 1
}

$cargoHome = $env:CARGO_HOME
if (-not $cargoHome -and $env:USERPROFILE) { $cargoHome = [IO.Path]::Combine($env:USERPROFILE, '.cargo') }
$cargoBin = $null
if ($cargoHome -match '^[A-Za-z]:[\\/]') {
    try { $cargoBin = [IO.Path]::GetFullPath([IO.Path]::Combine($cargoHome, 'bin')) } catch {}
}

$results = @{}
$reportedPaths = @{}
foreach ($entry in ($env:PATH -split ';')) {
    $directory = $entry.Trim().Trim('"')
    if ($directory -notmatch '^[A-Za-z]:[\\/]' -or $directory.Substring(2).Contains(':')) { continue }
    try {
        $directory = [IO.Path]::GetFullPath($directory)
        $directoryLock = Open-PlainDirectory $directory
    } catch { continue }

    if (-not $directoryLock.Handles) {
        if ($directoryLock.Reason -eq 'a reparse point was found' -and -not $reportedPaths.ContainsKey($directory)) {
            Write-Output "PATH: SKIPPED [$directory]. Reparse point [$($directoryLock.BlockedPath)]."
            $reportedPaths[$directory] = $true
        }
        continue
    }
    try {
        foreach ($tool in $tools) {
            if ($results.ContainsKey($tool.Name)) { continue }
            $candidate = Get-ToolEntry $directory $tool.Name
            if (-not $candidate) { continue }
            $results[$tool.Name] = $candidate
            if ($candidate.Reason) { continue }

            $fileLock = Open-PlainPathItem $candidate.Path $false
            if ($fileLock.Reason) {
                $candidate.Reason = $fileLock.Reason
                continue
            }
            try {
                $candidate.Reason = Get-ProxyReason $tool.Name $candidate.Path $directory
                if (-not $candidate.Reason) {
                    $probe = Get-LocalVersion $tool $candidate.Path $directory
                    $candidate.Reason = $probe.Reason
                    $candidate.Version = $probe.Version
                }
            } catch {
                $candidate.Reason = 'the local executable could not be verified'
            } finally {
                Close-PathHandles $fileLock.Handle
            }
        }
    } finally {
        Close-PathHandles $directoryLock.Handles
    }
    if ($results.Count -eq $tools.Count) { break }
}

$checkFailed = $false
foreach ($tool in $tools) {
    $candidate = $results[$tool.Name]
    if (-not $candidate) {
        Write-Output "$($tool.Name): MISSING. $($tool.Hint)"
        $checkFailed = $true
    } elseif ($candidate.Reason -or -not $candidate.Version) {
        Write-Output "$($tool.Name): UNVERIFIED [$($candidate.Path)]. $($candidate.Reason). $($tool.Hint)"
        $checkFailed = $true
    } elseif ($Locked -and $candidate.Version -cne $pinned.($tool.Name)) {
        Write-Output "$($tool.Name): VERSION MISMATCH [$($candidate.Path)] ($($candidate.Version)). Expected $($pinned.($tool.Name))."
        $checkFailed = $true
    } elseif ($tool.Target -and $candidate.Version -notmatch $tool.Target) {
        Write-Output "$($tool.Name): VERSION MISMATCH [$($candidate.Path)] ($($candidate.Version)). $($tool.Hint)"
        $checkFailed = $true
    } else {
        Write-Output "$($tool.Name): OK [$($candidate.Path)] ($($candidate.Version))"
    }
}

if ($checkFailed) { exit 1 }
exit 0
