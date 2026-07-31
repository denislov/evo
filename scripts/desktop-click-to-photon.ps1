[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$artifactDir = Join-Path $repositoryRoot "target/desktop-perf"
$logFile = Join-Path $artifactDir "click-to-photon-app-latest.log"
$minimumSamples = 50
$isWindowsHost = [System.IO.Path]::DirectorySeparatorChar -eq '\'
$binaryName = if ($isWindowsHost) { "desktop.exe" } else { "desktop" }
$binary = Join-Path $repositoryRoot "target/release/$binaryName"

New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
Set-Location $repositoryRoot

& cargo build -p desktop --release --features desktop-devtools
if ($LASTEXITCODE -ne 0) {
    throw "release desktop build failed with exit code $LASTEXITCODE"
}

$env:EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY = "1"
Write-Host "Press Space at least $minimumSamples times while the external sensor records matching sample IDs; press Escape only after the final post-render sample."
Write-Host "The external CSV must contain run_id,sample_id,latency_us and use the run ID printed by this replay."
& $binary 2>&1 | Tee-Object -FilePath $logFile
if ($LASTEXITCODE -ne 0) {
    throw "click-to-photon replay failed with exit code $LASTEXITCODE"
}

$sampleRows = @(
    Get-Content -Path $logFile | ForEach-Object {
        if ($_ -match "desktop_trace`tclick_to_photon_post_render`trun=(?<run>[A-Za-z0-9][A-Za-z0-9._-]{0,127})`tsample=(?<sample>[0-9]+)`t") {
            [PSCustomObject]@{
                RunId = $Matches["run"]
                SampleId = [UInt64]$Matches["sample"]
            }
        }
    }
)
$runIds = @($sampleRows.RunId | Sort-Object -Unique)
if ($runIds.Count -ne 1) {
    throw "click-to-photon replay must emit exactly one run ID; found $($runIds.Count)"
}
$runId = $runIds[0]
$sampleCount = @(
    $sampleRows |
        Where-Object { $_.RunId -eq $runId } |
        Select-Object -ExpandProperty SampleId -Unique
).Count
if ($sampleCount -lt $minimumSamples) {
    throw "click-to-photon replay requires at least $minimumSamples post-render samples; found $sampleCount"
}
Write-Host "click-to-photon replay captured $sampleCount unique post-render samples for run $runId in $logFile"
