[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Run,
    [Parameter(Mandatory = $true)][string]$WorkerFolder,
    [Parameter(Mandatory = $true)][string]$VideoToolsEvidence
)

$ErrorActionPreference = 'Stop'
$tools = Get-Content -LiteralPath $VideoToolsEvidence -Encoding utf8 -Raw | ConvertFrom-Json
$ffmpeg = (Resolve-Path -LiteralPath $tools.ffmpeg).ProviderPath
$ffprobe = (Resolve-Path -LiteralPath $tools.ffprobe).ProviderPath
$bin = Split-Path -Parent $ffmpeg
if ((Split-Path -Leaf $ffmpeg) -ne 'ffmpeg.exe' -or (Split-Path -Leaf $ffprobe) -ne 'ffprobe.exe' -or
    (Split-Path -Parent $ffprobe) -ne $bin) {
    throw 'Expected the verified FFmpeg and ffprobe tools from one Windows binary directory.'
}
$destination = Join-Path $WorkerFolder 'video-tools'
New-Item -ItemType Directory -Path $destination -Force | Out-Null
$binaries = @($ffmpeg, $ffprobe) + @(Get-ChildItem -LiteralPath $bin -Filter '*.dll' -File | ForEach-Object FullName)
foreach ($binary in $binaries) {
    $copy = Join-Path $destination (Split-Path -Leaf $binary)
    Copy-Item -LiteralPath $binary -Destination $copy
    if ((Get-FileHash -LiteralPath $copy).Hash -ne (Get-FileHash -LiteralPath $binary).Hash) {
        throw "Video tool changed while copying the runtime layout: $copy"
    }
}
Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $bin) 'LICENSE.txt') -Destination $destination
$source = Join-Path $Run 'Synthetic motion.mkv'
$reference = Join-Path $Run 'expected-frame-2.png'
& $ffmpeg -v error -nostdin -f lavfi -i 'testsrc2=size=32x24:rate=2' -frames:v 6 -c:v ffv1 $source
if ($LASTEXITCODE -ne 0) { throw 'Synthetic video generation failed.' }
& $ffmpeg -v error -nostdin -i $source -vf 'select=eq(n\,2)' -frames:v 1 -pix_fmt rgb24 $reference
if ($LASTEXITCODE -ne 0) { throw 'Reference frame decoding failed.' }
[pscustomobject]@{
    Source = $source
    Reference = $reference
    SourceDigest = (Get-FileHash -LiteralPath $source).Hash
}
