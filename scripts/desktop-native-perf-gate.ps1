[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$artifactDir = Join-Path $repositoryRoot "target/desktop-perf"
$logFile = Join-Path $artifactDir "native-latest.log"
$isWindowsHost = [System.IO.Path]::DirectorySeparatorChar -eq '\'
$binaryName = if ($isWindowsHost) { "desktop.exe" } else { "desktop" }
$binary = Join-Path $repositoryRoot "target/release/$binaryName"

New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
Set-Location $repositoryRoot

function Get-Percentile {
    param(
        [Parameter(Mandatory = $true)][double[]]$Samples,
        [Parameter(Mandatory = $true)][ValidateRange(1, 100)][int]$Percentile
    )

    if ($Samples.Count -eq 0) {
        throw "percentile requires at least one sample"
    }
    $ordered = @($Samples | Sort-Object)
    $index = [Math]::Ceiling($ordered.Count * $Percentile / 100.0) - 1
    return $ordered[$index]
}

function Get-TabMetric {
    param(
        [Parameter(Mandatory = $true)][string]$Line,
        [Parameter(Mandatory = $true)][string]$Name
    )

    foreach ($field in ($Line -split "`t")) {
        $pair = $field -split "=", 2
        if ($pair.Count -eq 2 -and $pair[0] -eq $Name) {
            return $pair[1]
        }
    }
    return $null
}

& cargo build -p desktop --release --features desktop-devtools
if ($LASTEXITCODE -ne 0) {
    throw "release desktop build failed with exit code $LASTEXITCODE"
}

$env:ZED_MEASUREMENTS = "1"
$env:EVO_DESKTOP_NATIVE_PERF_REPLAY = "1"
$env:EVO_DESKTOP_MARKDOWN_TRACE = "1"
& $binary 2>&1 | Tee-Object -FilePath $logFile
if ($LASTEXITCODE -ne 0) {
    throw "native desktop replay failed with exit code $LASTEXITCODE"
}

$lines = @(Get-Content -Path $logFile)
$frameSamples = [System.Collections.Generic.List[double]]::new()
foreach ($line in $lines) {
    if ($line -notmatch '^frame duration:\s+([0-9.]+)(ms|µs|us|ns|s)$') {
        continue
    }
    $value = [double]$Matches[1]
    switch ($Matches[2]) {
        "ms" { $frameSamples.Add($value * 1000.0) }
        "µs" { $frameSamples.Add($value) }
        "us" { $frameSamples.Add($value) }
        "ns" { $frameSamples.Add($value / 1000.0) }
        "s"  { $frameSamples.Add($value * 1000000.0) }
    }
}

$frameSamples = @($frameSamples | Select-Object -Last 200)
if ($frameSamples.Count -ne 200) {
    throw "expected 200 native frame-duration samples, found $($frameSamples.Count)"
}
$frameP95 = [Math]::Round((Get-Percentile -Samples $frameSamples -Percentile 95))
$frameP99 = [Math]::Round((Get-Percentile -Samples $frameSamples -Percentile 99))
$frameP95Budget = 16700
$frameP99Budget = 33000
$frameMetric = "desktop_perf`tnative_gpu_present_frame_p95_us=$frameP95`tnative_gpu_present_frame_p99_us=$frameP99`tnative_frame_p95_budget_us=$frameP95Budget`tnative_frame_p99_budget_us=$frameP99Budget"
$frameMetric | Tee-Object -FilePath $logFile -Append
if ($frameP95 -gt $frameP95Budget) {
    throw "native GPU/present frame P95 exceeded one frame: $frameP95 us"
}
if ($frameP99 -gt $frameP99Budget) {
    throw "native GPU/present frame P99 exceeded two frames: $frameP99 us"
}

$inputLine = $lines | Where-Object { $_ -match 'native_input_dispatch_to_post_render_p95_us=' } | Select-Object -First 1
if ($null -eq $inputLine) {
    throw "native input-to-post-render metrics were not emitted"
}
$inputSamples = [int](Get-TabMetric $inputLine "native_input_samples")
$inputP95 = [int](Get-TabMetric $inputLine "native_input_dispatch_to_post_render_p95_us")
$inputP99 = [int](Get-TabMetric $inputLine "native_input_dispatch_to_post_render_p99_us")
$inputP95Budget = 50000
$inputMetric = "desktop_perf`tnative_input_samples=$inputSamples`tnative_input_dispatch_to_post_render_p95_us=$inputP95`tnative_input_dispatch_to_post_render_p99_us=$inputP99`tnative_input_p95_budget_us=$inputP95Budget"
$inputMetric | Tee-Object -FilePath $logFile -Append
if ($inputSamples -ne 50) {
    throw "expected 50 paired native input samples, found $inputSamples"
}
if ($inputP95 -gt $inputP95Budget) {
    throw "native input dispatch-to-post-render P95 exceeded 50 ms: $inputP95 us"
}

$nativePlatform = Get-TabMetric $inputLine "platform"
$nativeRssSupported = Get-TabMetric $inputLine "native_rss_supported"
$nativeRssBefore = Get-TabMetric $inputLine "native_rss_before_bytes"
$nativeRssWarmup = Get-TabMetric $inputLine "native_rss_after_warmup_bytes"
$nativeRssAfter = Get-TabMetric $inputLine "native_rss_after_bytes"
$nativeRssStartupGrowth = Get-TabMetric $inputLine "native_rss_startup_growth_bytes"
$nativeRssSteadyGrowth = Get-TabMetric $inputLine "native_rss_steady_growth_bytes"
if (
    $null -eq $nativePlatform -or
    $nativeRssSupported -ne "true" -or
    $null -eq $nativeRssBefore -or
    $null -eq $nativeRssWarmup -or
    $null -eq $nativeRssAfter -or
    $null -eq $nativeRssStartupGrowth -or
    $null -eq $nativeRssSteadyGrowth
) {
    throw "native resident-memory probe is unavailable on this desktop platform"
}
$nativeRssAbsoluteBudget = 256 * 1024 * 1024
$nativeRssSteadyBudget = 64 * 1024 * 1024
$nativeRssMetric = "desktop_perf`tplatform=$nativePlatform`tnative_rss_supported=$nativeRssSupported`tnative_rss_before_bytes=$nativeRssBefore`tnative_rss_after_warmup_bytes=$nativeRssWarmup`tnative_rss_after_bytes=$nativeRssAfter`tnative_rss_startup_growth_bytes=$nativeRssStartupGrowth`tnative_rss_steady_growth_bytes=$nativeRssSteadyGrowth`tnative_rss_absolute_budget_bytes=$nativeRssAbsoluteBudget`tnative_rss_steady_budget_bytes=$nativeRssSteadyBudget"
$nativeRssMetric | Tee-Object -FilePath $logFile -Append
if ([uint64]$nativeRssAfter -gt $nativeRssAbsoluteBudget) {
    throw "native window RSS exceeded 256 MiB: $nativeRssAfter bytes"
}
if ([uint64]$nativeRssSteadyGrowth -gt $nativeRssSteadyBudget) {
    throw "native steady-state RSS growth exceeded 64 MiB: $nativeRssSteadyGrowth bytes"
}

$markdownSamples = [System.Collections.Generic.List[double]]::new()
foreach ($line in $lines) {
    if ($line -notmatch 'markdown_parse_complete') {
        continue
    }
    $value = Get-TabMetric $line "markdown_parse_to_layout_us"
    if ($null -ne $value) {
        $markdownSamples.Add([double]$value)
    }
}
if ($markdownSamples.Count -lt 1) {
    throw "production Markdown completion tracing emitted no samples"
}
$markdownP95 = [Math]::Round(
    (Get-Percentile -Samples ($markdownSamples.ToArray()) -Percentile 95)
)
$markdownP95Budget = 150000
$markdownMetric = "desktop_perf`tproduction_markdown_completion_samples=$($markdownSamples.Count)`tproduction_markdown_parse_to_layout_p95_us=$markdownP95`tproduction_markdown_p95_budget_us=$markdownP95Budget"
$markdownMetric | Tee-Object -FilePath $logFile -Append
if ($markdownP95 -gt $markdownP95Budget) {
    throw "production Markdown parse-to-layout P95 exceeded 150 ms: $markdownP95 us"
}

Write-Output "desktop native performance gate passed; log: $logFile"
