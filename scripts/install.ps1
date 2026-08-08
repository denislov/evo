[CmdletBinding()]
param(
    [ValidateSet('cli', 'desktop')]
    [string]$Component = 'cli',

    [string]$Version,

    [string]$InstallDir,

    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$Repository = 'denislov/evo'
$ReleasesUrl = "https://github.com/$Repository/releases"

function Show-Usage {
    @'
Install Evo from GitHub Releases.

Examples:
  irm https://raw.githubusercontent.com/denislov/evo/main/scripts/install.ps1 | iex
  .\install.ps1 -Component desktop
  .\install.ps1 -Component cli -Version 0.7.2 -InstallDir "$env:LOCALAPPDATA\Evo\bin"
'@ | Write-Output
}

if ($Help) {
    Show-Usage
    exit 0
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'This installer supports Windows x86_64 only.'
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $response = Invoke-WebRequest -UseBasicParsing -MaximumRedirection 8 "$ReleasesUrl/latest"
    $Version = Split-Path -Leaf $response.BaseResponse.ResponseUri.AbsolutePath
}
$Version = $Version -replace '^v', ''
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$') {
    throw "Invalid release version: $Version"
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Evo\bin'
}

$Archive = "evo-$Component-$Version-x86_64-pc-windows-msvc.zip"
$ReleaseUrl = "$ReleasesUrl/download/v$Version"
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("evo-install-" + [guid]::NewGuid().ToString('N'))
$ExtractDir = Join-Path $TempDir 'extract'

try {
    New-Item -ItemType Directory -Path $TempDir | Out-Null
    $ChecksumsPath = Join-Path $TempDir 'checksums.txt'
    $ArchivePath = Join-Path $TempDir $Archive
    Invoke-WebRequest -UseBasicParsing "$ReleaseUrl/checksums.txt" -OutFile $ChecksumsPath
    Invoke-WebRequest -UseBasicParsing "$ReleaseUrl/$Archive" -OutFile $ArchivePath

    $ExpectedLine = Get-Content -LiteralPath $ChecksumsPath | Where-Object { $_ -match "\s$([regex]::Escape($Archive))$" }
    if (@($ExpectedLine).Count -ne 1) {
        throw "checksums.txt does not contain exactly one digest for $Archive"
    }
    $ExpectedHash = ($ExpectedLine -split '\s+')[0].ToLowerInvariant()
    $ActualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArchivePath).Hash.ToLowerInvariant()
    if ($ExpectedHash -ne $ActualHash) {
        throw "SHA-256 verification failed for $Archive"
    }

    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $ExtractDir -Force
    $BinaryName = if ($Component -eq 'cli') { 'coding-agent.exe' } else { 'desktop.exe' }
    $BinaryPath = Join-Path $ExtractDir $BinaryName
    if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
        throw "Release archive did not contain expected binary: $BinaryName"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $InstallDir $BinaryName) -Force
    Write-Output "Installed $Component $Version to $(Join-Path $InstallDir $BinaryName)"
    Write-Output "Ensure $InstallDir is on PATH before running it."
}
finally {
    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
}
