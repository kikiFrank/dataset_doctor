$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..\..'))
$dist = Join-Path $repo 'dist'
$root = Join-Path $dist 'p00-worker-smoke'
$logs = Join-Path $root 'logs'
$fixtures = Join-Path $root 'fixtures'
$exe = Join-Path $root 'package\worker-smoke\worker-smoke.exe'
$utf8 = [Text.UTF8Encoding]::new($false)
$timeout = 10000

function Initialize-VerifyDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($item -and (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain verification directory: $Path"
    }
    [void][IO.Directory]::CreateDirectory($Path)
}

function Assert-PlainFile([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($item -and ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
        throw "Expected a plain file: $Path"
    }
}

function Send-Request($Writer, $Message) {
    $Writer.WriteLine((ConvertTo-Json -InputObject $Message -Compress))
}

function Read-Response($Process, $Case) {
    $read = $Process.StandardOutput.ReadLineAsync()
    if (-not $read.Wait($timeout)) { throw 'Timed out waiting for the Worker response.' }
    if ($null -eq $read.Result) { throw 'Worker closed stdout before responding.' }
    $response = ConvertFrom-Json -InputObject $read.Result
    $Case.responses.Add($response)
    return $response
}

function Invoke-WorkerCase($Case, [string]$ImagePath, [bool]$ExpectDecodeFailure) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $exe
    $start.WorkingDirectory = $fixtures
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $utf8
    $start.StandardErrorEncoding = $utf8
    foreach ($name in @($start.EnvironmentVariables.Keys)) {
        if ($name -match '^(PYTHON|PYINSTALLER|_PYI_|VIRTUAL_ENV|CONDA|_CONDA|_CE_|__PYVENV_LAUNCHER__)') {
            $start.EnvironmentVariables.Remove($name)
        }
    }
    $system = [Environment]::SystemDirectory
    $windows = Split-Path -Parent $system
    $start.EnvironmentVariables['PATH'] = "$system;$windows;$(Join-Path $system 'Wbem')"
    $start.EnvironmentVariables['TEMP'] = $fixtures
    $start.EnvironmentVariables['TMP'] = $fixtures
    $Case.child_path = $start.EnvironmentVariables['PATH']

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    $writer = $null
    $stderr = $null
    try {
        $started = $process.Start()
        if (-not $started) { throw 'Could not start the packaged Worker.' }
        $Case.pid = $process.Id
        $stderr = $process.StandardError.ReadToEndAsync()
        $writer = [IO.StreamWriter]::new($process.StandardInput.BaseStream, $utf8, 1024, $true)
        $writer.AutoFlush = $true

        Send-Request $writer @{ op = 'hello' }
        $ready = Read-Response $process $Case
        if ($ready.type -cne 'ready' -or $ready.worker -cne 'dataset-doctor-p00-02' -or
            $ready.python -cne '3.12.12' -or $ready.platform -cne 'win-amd64' -or
            $ready.pillow -cne '12.1.0' -or 'cpu-png' -cnotin $ready.capabilities) {
            throw 'Unexpected Worker handshake.'
        }

        Send-Request $writer @{ op = 'decode'; path = $ImagePath }
        $reply = Read-Response $process $Case
        if ($ExpectDecodeFailure) {
            if ($reply.type -cne 'error' -or $reply.code -cne 'decode_failed') {
                throw 'Corrupt PNG did not return the expected decode error.'
            }
        } else {
            $pixelJson = ConvertTo-Json -InputObject $reply.pixels -Compress -Depth 4
            if ($reply.type -cne 'decoded' -or $reply.width -ne 2 -or $reply.height -ne 1 -or
                $reply.mode -cne 'RGB' -or $pixelJson -cne '[[255,0,0],[0,0,255]]') {
                throw 'Decoded PNG did not match the expected 2x1 RGB pixels.'
            }
            Send-Request $writer @{ op = 'shutdown' }
            $bye = Read-Response $process $Case
            if ($bye.type -cne 'bye') { throw 'Worker did not acknowledge shutdown.' }
        }
        $writer.Dispose()
        $writer = $null
        $process.StandardInput.Close()

        $trailing = $process.StandardOutput.ReadToEndAsync()
        if (-not $process.WaitForExit($timeout) -or -not $trailing.Wait($timeout)) {
            throw 'Worker did not close its process and stdout within the timeout.'
        }
        if ($trailing.Result) { throw 'Unexpected trailing stdout from the Worker.' }
        $Case.exit_code = $process.ExitCode
        if (($ExpectDecodeFailure -and $Case.exit_code -eq 0) -or
            (-not $ExpectDecodeFailure -and $Case.exit_code -ne 0)) {
            throw "Unexpected Worker exit code $($Case.exit_code)."
        }
        $Case.status = 'passed'
    } finally {
        if ($writer) { try { $writer.Dispose() } catch {} }
        try {
            if ($started) {
                try { $process.StandardInput.Close() } catch {}
                if (-not $process.HasExited) {
                    try { $process.Kill() } catch {
                        if (-not $process.HasExited) { throw }
                    }
                    if (-not $process.WaitForExit($timeout)) { throw 'Worker did not exit after cancellation.' }
                }
                $Case.exit_code = $process.ExitCode
                if ($stderr -and $stderr.Wait($timeout)) { $Case.stderr = $stderr.Result }
            }
        } finally {
            $process.Dispose()
        }
    }
}

$evidence = [ordered]@{
    status = 'failed'
    git_head = $null
    verification_entry = 'powershell.exe -NoProfile -NonInteractive -File .\tests\packaging\windows\worker-smoke\verify-worker-smoke.ps1'
    exe = $exe
    working_directory = $fixtures
    fixtures = [ordered]@{}
    cases = [ordered]@{}
    error = $null
}
$log = $null
$exitCode = 1
try {
    foreach ($directory in @($dist, $root, $logs, $fixtures)) { Initialize-VerifyDirectory $directory }
    $logPath = Join-Path $logs 'verify.json'
    Assert-PlainFile $logPath
    $log = $logPath
    $git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
    $evidence.git_head = & $git -C $repo rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Could not record the repository HEAD.' }

    $sourcePng = Join-Path $repo 'testdata\image_io\p00-worker-rgb.png'
    $sampleName = "$([char]0x6837)$([char]0x672C)"
    $valid = Join-Path $fixtures "$sampleName valid.png"
    $corrupt = Join-Path $fixtures "$sampleName corrupt.png"
    Assert-PlainFile $valid
    Assert-PlainFile $corrupt
    $validBytes = [IO.File]::ReadAllBytes($sourcePng)
    # Preserve the complete IHDR and IDAT length/type, and retain only three IDAT payload bytes.
    if ($validBytes.Length -lt 44 -or [Text.Encoding]::ASCII.GetString($validBytes, 37, 4) -cne 'IDAT') {
        throw 'Unexpected layout of the tracked 2x1 PNG fixture.'
    }
    [IO.File]::WriteAllBytes($valid, $validBytes)
    [IO.File]::WriteAllBytes($corrupt, [byte[]]$validBytes[0..43])
    foreach ($caseName in @('valid', 'corrupt')) {
        $imagePath = if ($caseName -eq 'valid') { $valid } else { $corrupt }
        $evidence.fixtures[$caseName] = [ordered]@{
            path = $imagePath
            sha256 = (Get-FileHash -LiteralPath $imagePath -Algorithm SHA256).Hash
        }
    }

    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing packaged Worker EXE: $exe" }
    foreach ($directory in @((Join-Path $root 'package'), (Split-Path -Parent $exe))) {
        Initialize-VerifyDirectory $directory
    }
    Assert-PlainFile $exe
    foreach ($caseName in @('valid', 'corrupt')) {
        $case = [ordered]@{
            status = 'failed'
            child_path = $null
            pid = $null
            responses = [Collections.Generic.List[object]]::new()
            exit_code = $null
            stderr = $null
        }
        $evidence.cases[$caseName] = $case
        Invoke-WorkerCase $case $evidence.fixtures[$caseName].path ($caseName -eq 'corrupt')
    }
    $evidence.status = 'passed'
    $exitCode = 0
    Write-Host "Packaged Worker smoke verification passed. Evidence: $log"
} catch {
    $evidence.error = $_.Exception.Message
    [Console]::Error.WriteLine($evidence.error)
} finally {
    if ($log) { [IO.File]::WriteAllText($log, (ConvertTo-Json -InputObject $evidence -Depth 10) + [Environment]::NewLine, $utf8) }
}
exit $exitCode
