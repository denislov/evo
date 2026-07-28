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

    $output = @(& cargo @Arguments 2>&1)
    $exitCode = $LASTEXITCODE
    $output | Tee-Object -FilePath $logFile -Append
    if ($exitCode -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $exitCode"
    }
    $summary = $output -join [Environment]::NewLine
    if ($summary -notmatch "(?m)^running 1 test$") {
        throw "cargo $($Arguments -join ' ') did not run exactly one test"
    }
}

$releaseTests = @(
    "conversation::model::tests::desktop_release_empty_conversation_baseline",
    "conversation::model::tests::desktop_release_ten_mib_interaction_baseline",
    "conversation::model::tests::desktop_release_scale_content_and_streaming_matrix",
    "app::native_shell::tests::desktop_release_gpui_headless_frame_and_input_replay",
    "app::native_shell::tests::desktop_release_gpui_markdown_parser_matrix"
)

Set-Content -Path $logFile -Value ""
foreach ($testName in $releaseTests) {
    Invoke-CargoGate -Arguments @(
        "test", "-p", "desktop", "--lib", "--release", $testName, "--",
        "--ignored", "--exact", "--nocapture", "--test-threads=1"
    )
}

Write-Output "desktop performance gate passed; log: $logFile"
