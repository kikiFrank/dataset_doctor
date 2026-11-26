# P00-01: inspect the existing Windows toolchain without installing anything.
$tools = @(
    @{ Name = 'cargo'; Args = '--version'; Pattern = '^cargo (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the bin directory of an already installed Rust toolchain.' }
    @{ Name = 'rustc'; Args = '--version'; Pattern = '^rustc (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the bin directory of an already installed Rust toolchain.' }
    @{ Name = 'node'; Args = '--version'; Pattern = '^v(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$'; Target = '^24\.\d+\.\d+$'; Hint = 'Expose a local Node 24 release executable.' }
    @{ Name = 'pnpm'; Args = '--version'; Pattern = '^(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$'; Hint = 'Expose a locally installed standalone pnpm.exe.' }
    @{ Name = 'python'; Args = '-E -S -B -c "import sys; print(''%s %s'' % (getattr(getattr(sys, ''implementation'', None), ''name'', ''python''), sys.version.split()[0]))"'; Pattern = '^(?<version>[a-z]+ \d+\.\d+\.\d+[a-z0-9]*)$'; Target = '^cpython 3\.12\.\d+$'; Hint = 'Expose a local CPython 3.12 release executable.' }
    @{ Name = 'uv'; Args = '--version'; Pattern = '^uv (?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\s|$)'; Hint = 'Expose the local uv executable.' }
    @{ Name = 'protoc'; Args = '--version'; Pattern = '^libprotoc (?<version>\d+(?:\.\d+){1,2})(?:\s|$)'; Hint = 'Expose the local protoc executable.' }
)

# Only absolute local PATH entries apply. Ignore the caller's working directory.
$localPath = @(
    foreach ($entry in ($env:PATH -split ';')) {
        $directory = $entry.Trim().Trim('"')
        if ($directory -notmatch '^[A-Za-z]:[\\/]') { continue }
        try {
            $directory = [IO.Path]::GetFullPath($directory)
            $driveType = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($directory)).DriveType
            if ($driveType -in @('Fixed', 'Removable', 'RAMDisk', 'CDRom')) { $directory }
        } catch {
            # An invalid or inaccessible PATH entry cannot supply a local tool.
        }
    }
)

$cargoHome = $env:CARGO_HOME
if (-not $cargoHome -and $env:USERPROFILE) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
$cargoBin = $null
if ($cargoHome -match '^[A-Za-z]:[\\/]') {
    try { $cargoBin = [IO.Path]::GetFullPath([IO.Path]::Combine($cargoHome, 'bin')) } catch {}
}

function Find-LocalTool($name) {
    foreach ($directory in $localPath) {
        foreach ($extension in @('.exe', '.cmd', '.bat', '.ps1', '.com')) {
            $path = [IO.Path]::Combine($directory, "$name$extension")
            $file = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
            if ($file -and -not $file.PSIsContainer) { return $file }
        }
    }
}

function Get-ProxyReason($tool, $file) {
    if ($file.Extension -ine '.exe') { return 'a command shim was found' }
    if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { return 'an executable link or app alias was found' }
    if ($file.FullName -match '[\\/](WindowsApps|corepack|shims|\.volta)[\\/]') { return 'a tool manager or app alias path was found' }
    if ($tool.Name -in @('cargo', 'rustc')) {
        $rustup = [IO.Path]::Combine($file.DirectoryName, 'rustup.exe')
        if ($file.DirectoryName -ieq $cargoBin -or [IO.File]::Exists($rustup)) { return 'a possible Rustup proxy was found' }
    }

    $metadata = $file.VersionInfo
    $description = @($metadata.OriginalFilename, $metadata.ProductName, $metadata.FileDescription) -join ' '
    if ($description -match 'rustup|corepack|volta|shimgen|shim launcher') { return 'a tool manager or shim was found' }
}

function Get-LocalVersion($tool, $file) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $file.FullName
    $start.Arguments = $tool.Args
    $start.WorkingDirectory = [Environment]::SystemDirectory
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['PATH'] = $localPath -join ';'

    # Avoid Node startup hooks and package manager downloads for version queries.
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

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { return }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(5000)) {
            $process.Kill()
            return
        }

        # Child output can include unrelated data. Print only the matched version.
        if ($process.ExitCode -eq 0 -and $stdout.Wait(1000) -and $stderr.Wait(1000)) {
            $versionMatch = [regex]::Match($stdout.Result.Trim(), $tool.Pattern)
            if ($versionMatch.Success) { return $versionMatch.Groups['version'].Value }
        }
    } catch {
        # Launch, exit, timeout and output failures leave the version unverified.
    } finally {
        $process.Dispose()
    }
}

$checkFailed = $false
foreach ($tool in $tools) {
    $file = Find-LocalTool $tool.Name
    if (-not $file) {
        Write-Output "$($tool.Name): MISSING. $($tool.Hint)"
        $checkFailed = $true
        continue
    }

    $path = $file.FullName
    $proxyReason = Get-ProxyReason $tool $file
    if ($proxyReason) {
        Write-Output "$($tool.Name): UNVERIFIED [$path]. $proxyReason. $($tool.Hint)"
        $checkFailed = $true
        continue
    }

    $version = Get-LocalVersion $tool $file
    if (-not $version) {
        Write-Output "$($tool.Name): UNVERIFIED [$path]. No version was confirmed by the local probe. $($tool.Hint)"
        $checkFailed = $true
    } elseif ($tool.Target -and $version -notmatch $tool.Target) {
        Write-Output "$($tool.Name): VERSION MISMATCH [$path] ($version). $($tool.Hint)"
        $checkFailed = $true
    } else {
        Write-Output "$($tool.Name): OK [$path] ($version)"
    }
}

if ($checkFailed) { exit 1 }
exit 0
