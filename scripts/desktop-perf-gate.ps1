[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$artifactDir = Join-Path $repositoryRoot "target/desktop-perf"
$logFile = Join-Path $artifactDir "latest.log"
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
Set-Location $repositoryRoot

function Invoke-CargoGate {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    & cargo @Arguments 2>&1 | Tee-Object -FilePath $logFile -Append
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

Set-Content -Path $logFile -Value ""
Invoke-CargoGate -Arguments @(
    "test", "-p", "desktop", "--lib", "--release",
    "conversation::tests::desktop_release_", "--",
    "--ignored", "--nocapture", "--test-threads=1"
)
Invoke-CargoGate -Arguments @(
    "test", "-p", "desktop", "--lib", "--release",
    "app::native_shell::tests::desktop_release_gpui_", "--",
    "--ignored", "--nocapture", "--test-threads=1"
)

Write-Output "desktop performance gate passed; log: $logFile"
