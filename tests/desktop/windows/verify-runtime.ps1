[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$DesktopExe)

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
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(25000)) {
            $process.Kill()
            throw 'The native desktop did not acknowledge the WebView2 IPC handshake.'
        }
        if (-not $stdout.Wait(2000) -or -not $stderr.Wait(2000)) {
            throw 'The desktop process did not close its output streams.'
        }
        $stdout.Result, $stderr.Result | Set-Content -LiteralPath $log -Encoding UTF8
        if ($process.ExitCode -ne 0 -or $stdout.Result -notmatch '(?m)^DESKTOP_WEBVIEW2_READY version=0\.1\.0\s*$') {
            throw "Desktop exit=$($process.ExitCode). No matching WebView2 verification. See $log."
        }
        "Native desktop WebView2 loaded the built React UI, completed Tauri IPC and exited: $log"
    } finally {
        $process.Dispose()
    }
} catch {
    $_.Exception.Message | Add-Content -LiteralPath $log -Encoding UTF8
    throw "Desktop runtime check failed. See $log. $($_.Exception.Message)"
}
