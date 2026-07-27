[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$artifactDir = Join-Path $repositoryRoot "target/desktop-perf"
$logFile = Join-Path $artifactDir "click-to-photon-app-latest.log"
$isWindowsHost = [System.IO.Path]::DirectorySeparatorChar -eq '\'
$binaryName = if ($isWindowsHost) { "desktop.exe" } else { "desktop" }
$binary = Join-Path $repositoryRoot "target/release/$binaryName"

New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
Set-Location $repositoryRoot

& cargo build -p desktop --release
if ($LASTEXITCODE -ne 0) {
    throw "release desktop build failed with exit code $LASTEXITCODE"
}

$env:EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY = "1"
& $binary 2>&1 | Tee-Object -FilePath $logFile
if ($LASTEXITCODE -ne 0) {
    throw "click-to-photon replay failed with exit code $LASTEXITCODE"
}
