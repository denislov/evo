//! GitHub Release discovery and SHA-256 verification shared by Evo clients.
//!
//! This crate deliberately owns only the release protocol: latest-release
//! lookup, supported-platform asset selection, semantic version comparison and
//! archive integrity verification. CLI and Desktop retain their respective
//! installation and user-interaction responsibilities.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tempfile::{Builder as TempDirBuilder, TempDir};

pub const DEFAULT_REPOSITORY: &str = "denislov/evo";
const CHECKSUMS_ASSET: &str = "checksums.txt";
const GITHUB_API: &str = "https://api.github.com";
const MAX_RELEASE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseComponent {
    Cli,
    Desktop,
}

impl ReleaseComponent {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Desktop => "desktop",
        }
    }

    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::Cli => "coding-agent",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasePlatform {
    LinuxX86_64,
    WindowsX86_64,
}

impl ReleasePlatform {
    pub const fn current() -> Result<Self, UpdateError> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            return Ok(Self::LinuxX86_64);
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            return Ok(Self::WindowsX86_64);
        }
        #[allow(unreachable_code)]
        Err(UpdateError::UnsupportedPlatform {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        })
    }

    pub const fn archive_suffix(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu.tar.gz",
            Self::WindowsX86_64 => "x86_64-pc-windows-msvc.zip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableRelease {
    pub version: Version,
    pub archive: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

/// A verified release archive unpacked beside the executable it will replace.
///
/// Keeping the staging directory in the target's parent ensures the Unix
/// replacement is a same-filesystem rename rather than a copy/delete gap.
pub struct StagedRelease {
    release: AvailableRelease,
    _directory: TempDir,
    binary: PathBuf,
}

impl fmt::Debug for StagedRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedRelease")
            .field("version", &self.release.version)
            .field("binary", &self.binary)
            .finish_non_exhaustive()
    }
}

impl StagedRelease {
    pub fn version(&self) -> &Version {
        &self.release.version
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Replace a running executable on Unix, or schedule the equivalent
    /// replacement after the current process exits on Windows.
    pub fn install_over(&self, target: &Path) -> Result<InstallOutcome, UpdateError> {
        let metadata = fs::symlink_metadata(target).map_err(|source| UpdateError::Io {
            path: target.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(UpdateError::UnsafeInstallationTarget(target.to_path_buf()));
        }
        let next = self.copy_as_next_to(target)?;

        #[cfg(target_os = "windows")]
        {
            return schedule_windows_replacement(target, &next);
        }

        #[cfg(not(target_os = "windows"))]
        {
            if let Err(source) = fs::rename(&next, target) {
                let _ = fs::remove_file(&next);
                return Err(UpdateError::Io {
                    path: target.to_path_buf(),
                    source,
                });
            }
            Ok(InstallOutcome::Replaced)
        }
    }

    fn copy_as_next_to(&self, target: &Path) -> Result<PathBuf, UpdateError> {
        let parent = target
            .parent()
            .ok_or_else(|| UpdateError::MissingInstallationParent(target.to_path_buf()))?;
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UpdateError::UnsafeInstallationTarget(target.to_path_buf()))?;
        let next = parent.join(format!(
            ".{file_name}.evo-update-{}.new",
            std::process::id()
        ));
        let mut source = File::open(&self.binary).map_err(|source| UpdateError::Io {
            path: self.binary.clone(),
            source,
        })?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&next)
            .map_err(|source| UpdateError::Io {
                path: next.clone(),
                source,
            })?;
        if let Err(source) =
            io::copy(&mut source, &mut destination).and_then(|_| destination.sync_all())
        {
            let _ = fs::remove_file(&next);
            return Err(UpdateError::Io { path: next, source });
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&next, fs::Permissions::from_mode(0o755)).map_err(|source| {
                let _ = fs::remove_file(&next);
                UpdateError::Io {
                    path: next.clone(),
                    source,
                }
            })?;
        }
        Ok(next)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Replaced,
    /// A detached PowerShell helper is waiting for the caller to exit before
    /// replacing the locked executable.
    WindowsReplacementScheduled {
        helper: PathBuf,
    },
}

impl AvailableRelease {
    pub fn is_newer_than(&self, current: &Version) -> bool {
        self.version > *current
    }
}

#[derive(Clone)]
pub struct ReleaseClient {
    client: reqwest::Client,
    repository: String,
    api_base: String,
}

impl fmt::Debug for ReleaseClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseClient")
            .field("repository", &self.repository)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl ReleaseClient {
    pub fn new() -> Result<Self, UpdateError> {
        Self::for_repository(DEFAULT_REPOSITORY)
    }

    pub fn for_repository(repository: impl Into<String>) -> Result<Self, UpdateError> {
        Self::with_api_base(repository, GITHUB_API)
    }

    pub fn with_api_base(
        repository: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Result<Self, UpdateError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("evo-release-updater"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(UpdateError::HttpClient)?;
        Ok(Self {
            client,
            repository: repository.into(),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
        })
    }

    pub async fn latest(
        &self,
        component: ReleaseComponent,
        platform: ReleasePlatform,
    ) -> Result<AvailableRelease, UpdateError> {
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.api_base, self.repository
        );
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(UpdateError::Request)?;
        if !response.status().is_success() {
            return Err(UpdateError::ReleaseStatus(response.status().as_u16()));
        }
        let release = response
            .json::<GitHubRelease>()
            .await
            .map_err(UpdateError::ReleaseJson)?;
        AvailableRelease::from_github(release, component, platform)
    }

    /// Download, verify and unpack the latest supported release beside the
    /// executable it will replace. Call [`StagedRelease::install_over`] only
    /// after user intent has been obtained by the CLI or Desktop adapter.
    pub async fn stage_latest_for(
        &self,
        component: ReleaseComponent,
        platform: ReleasePlatform,
        installation_target: &Path,
    ) -> Result<StagedRelease, UpdateError> {
        let release = self.latest(component, platform).await?;
        let checksums = self.download(&release.checksums).await?;
        let archive = self.download(&release.archive).await?;
        self.verify_archive(&release, &checksums, &archive)?;
        stage_verified_archive(release, archive, installation_target)
    }

    /// Download an asset after first verifying its declared release size.
    ///
    /// The release's checksum is verified separately by [`Self::verify_archive`]
    /// so callers can stage its bytes wherever platform installation requires.
    pub async fn download(&self, asset: &ReleaseAsset) -> Result<Vec<u8>, UpdateError> {
        if asset.size > MAX_RELEASE_BYTES {
            return Err(UpdateError::AssetTooLarge {
                name: asset.name.clone(),
                bytes: asset.size,
            });
        }
        let response = self
            .client
            .get(&asset.download_url)
            .send()
            .await
            .map_err(UpdateError::Request)?;
        if !response.status().is_success() {
            return Err(UpdateError::AssetStatus {
                name: asset.name.clone(),
                status: response.status().as_u16(),
            });
        }
        if let Some(length) = response.content_length()
            && length > MAX_RELEASE_BYTES
        {
            return Err(UpdateError::AssetTooLarge {
                name: asset.name.clone(),
                bytes: length,
            });
        }
        let bytes = response.bytes().await.map_err(UpdateError::Request)?;
        if bytes.len() as u64 > MAX_RELEASE_BYTES {
            return Err(UpdateError::AssetTooLarge {
                name: asset.name.clone(),
                bytes: bytes.len() as u64,
            });
        }
        Ok(bytes.to_vec())
    }

    pub fn verify_archive(
        &self,
        release: &AvailableRelease,
        checksum_file: &[u8],
        archive: &[u8],
    ) -> Result<(), UpdateError> {
        let expected = checksum_for(checksum_file, &release.archive.name)?;
        let actual = hex_digest(archive);
        if expected != actual {
            return Err(UpdateError::ChecksumMismatch {
                name: release.archive.name.clone(),
                expected,
                actual,
            });
        }
        Ok(())
    }
}

fn stage_verified_archive(
    release: AvailableRelease,
    archive: Vec<u8>,
    installation_target: &Path,
) -> Result<StagedRelease, UpdateError> {
    let parent = installation_target
        .parent()
        .ok_or_else(|| UpdateError::MissingInstallationParent(installation_target.to_path_buf()))?;
    let directory = TempDirBuilder::new()
        .prefix(".evo-update-")
        .tempdir_in(parent)
        .map_err(|source| UpdateError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    let archive_path = directory.path().join(&release.archive.name);
    fs::write(&archive_path, archive).map_err(|source| UpdateError::Io {
        path: archive_path.clone(),
        source,
    })?;
    let extraction = directory.path().join("extract");
    fs::create_dir(&extraction).map_err(|source| UpdateError::Io {
        path: extraction.clone(),
        source,
    })?;
    extract_archive(&archive_path, &extraction)?;
    let binary = extraction.join(release_binary_name(&release.archive.name)?);
    let metadata = fs::symlink_metadata(&binary).map_err(|source| UpdateError::Io {
        path: binary.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(UpdateError::InvalidArchiveBinary(binary));
    }
    Ok(StagedRelease {
        release,
        _directory: directory,
        binary,
    })
}

fn release_binary_name(archive_name: &str) -> Result<&'static str, UpdateError> {
    if archive_name.starts_with("evo-cli-") {
        #[cfg(target_os = "windows")]
        return Ok("coding-agent.exe");
        #[cfg(not(target_os = "windows"))]
        return Ok("coding-agent");
    }
    if archive_name.starts_with("evo-desktop-") {
        #[cfg(target_os = "windows")]
        return Ok("desktop.exe");
        #[cfg(not(target_os = "windows"))]
        return Ok("desktop");
    }
    Err(UpdateError::InvalidArchiveName(archive_name.to_owned()))
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg("Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force")
            .arg(archive)
            .arg(destination);
        command
    };
    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut command = Command::new("tar");
        command
            .args(["--extract", "--gzip", "--file"])
            .arg(archive)
            .args(["--directory"])
            .arg(destination);
        command
    };
    let status = command
        .status()
        .map_err(|source| UpdateError::ArchiveTool {
            program: command.get_program().to_string_lossy().into_owned(),
            source,
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateError::ArchiveExtraction(status.code()))
    }
}

#[cfg(target_os = "windows")]
fn schedule_windows_replacement(target: &Path, next: &Path) -> Result<InstallOutcome, UpdateError> {
    let parent = target
        .parent()
        .ok_or_else(|| UpdateError::MissingInstallationParent(target.to_path_buf()))?;
    let helper = parent.join(format!(".evo-update-{}.ps1", std::process::id()));
    let escaped_target = target.display().to_string().replace('\'', "''");
    let escaped_next = next.display().to_string().replace('\'', "''");
    let helper_source = format!(
        "$pidToWaitFor = {}\nwhile (Get-Process -Id $pidToWaitFor -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 100 }}\nMove-Item -LiteralPath '{}' -Destination '{}' -Force\nRemove-Item -LiteralPath $PSCommandPath -Force\n",
        std::process::id(),
        escaped_next,
        escaped_target
    );
    fs::write(&helper, helper_source).map_err(|source| UpdateError::Io {
        path: helper.clone(),
        source,
    })?;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper)
        .spawn()
        .map_err(|source| UpdateError::ArchiveTool {
            program: "powershell.exe".into(),
            source,
        })?;
    Ok(InstallOutcome::WindowsReplacementScheduled { helper })
}

impl AvailableRelease {
    fn from_github(
        release: GitHubRelease,
        component: ReleaseComponent,
        platform: ReleasePlatform,
    ) -> Result<Self, UpdateError> {
        if release.draft || release.prerelease {
            return Err(UpdateError::NonStableRelease);
        }
        let version = Version::parse(release.tag_name.trim_start_matches('v'))
            .map_err(|_| UpdateError::InvalidVersion(release.tag_name))?;
        let archive_name = format!(
            "evo-{}-{}-{}",
            component.key(),
            version,
            platform.archive_suffix()
        );
        let archive = release
            .assets
            .iter()
            .find(|asset| asset.name == archive_name)
            .map(ReleaseAsset::from_github)
            .ok_or_else(|| UpdateError::MissingAsset(archive_name))?;
        let checksums = release
            .assets
            .iter()
            .find(|asset| asset.name == CHECKSUMS_ASSET)
            .map(ReleaseAsset::from_github)
            .ok_or_else(|| UpdateError::MissingAsset(CHECKSUMS_ASSET.into()))?;
        Ok(Self {
            version,
            archive,
            checksums,
        })
    }
}

impl ReleaseAsset {
    fn from_github(asset: &GitHubAsset) -> Self {
        Self {
            name: asset.name.clone(),
            download_url: asset.browser_download_url.clone(),
            size: asset.size,
        }
    }
}

fn checksum_for(bytes: &[u8], asset_name: &str) -> Result<String, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|_| UpdateError::InvalidChecksums)?;
    let mut matched = None;
    for line in text.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            continue;
        };
        if name != asset_name {
            continue;
        }
        if !is_sha256(digest) {
            return Err(UpdateError::InvalidChecksum {
                name: asset_name.to_owned(),
            });
        }
        if matched.replace(digest.to_ascii_lowercase()).is_some() {
            return Err(UpdateError::DuplicateChecksum {
                name: asset_name.to_owned(),
            });
        }
    }
    matched.ok_or_else(|| UpdateError::MissingChecksum {
        name: asset_name.to_owned(),
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("release updater does not support {os} {arch}")]
    UnsupportedPlatform {
        os: &'static str,
        arch: &'static str,
    },
    #[error("cannot create GitHub HTTP client: {0}")]
    HttpClient(reqwest::Error),
    #[error("GitHub release request failed: {0}")]
    Request(reqwest::Error),
    #[error("GitHub latest-release endpoint returned HTTP {0}")]
    ReleaseStatus(u16),
    #[error("GitHub latest-release response was invalid: {0}")]
    ReleaseJson(reqwest::Error),
    #[error("latest GitHub release is draft or prerelease")]
    NonStableRelease,
    #[error("GitHub release tag is not a supported semantic version: {0}")]
    InvalidVersion(String),
    #[error("GitHub release is missing required asset: {0}")]
    MissingAsset(String),
    #[error("GitHub asset {name} returned HTTP {status}")]
    AssetStatus { name: String, status: u16 },
    #[error("GitHub asset {name} exceeds the 1 GiB download limit ({bytes} bytes)")]
    AssetTooLarge { name: String, bytes: u64 },
    #[error("checksums.txt is not valid UTF-8")]
    InvalidChecksums,
    #[error("checksums.txt has no checksum for {name}")]
    MissingChecksum { name: String },
    #[error("checksums.txt has duplicate checksums for {name}")]
    DuplicateChecksum { name: String },
    #[error("checksums.txt has an invalid SHA-256 digest for {name}")]
    InvalidChecksum { name: String },
    #[error("SHA-256 mismatch for {name}: expected {expected}, received {actual}")]
    ChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("installation target has no parent directory: {0}")]
    MissingInstallationParent(PathBuf),
    #[error("installation target is not a regular non-symlink file: {0}")]
    UnsafeInstallationTarget(PathBuf),
    #[error("release archive has an unsupported name: {0}")]
    InvalidArchiveName(String),
    #[error("release archive did not unpack a regular binary at {0}")]
    InvalidArchiveBinary(PathBuf),
    #[error("cannot execute archive extraction program {program}: {source}")]
    ArchiveTool {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("archive extraction failed with exit code {0:?}")]
    ArchiveExtraction(Option<i32>),
    #[error("I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GitHubAsset {
        GitHubAsset {
            name: name.into(),
            browser_download_url: format!("https://example.test/{name}"),
            size: 42,
        }
    }

    #[test]
    fn release_selection_requires_the_exact_supported_asset_and_checksums() {
        let release = GitHubRelease {
            tag_name: "v0.7.2".into(),
            draft: false,
            prerelease: false,
            assets: vec![
                asset("evo-cli-0.7.2-x86_64-unknown-linux-gnu.tar.gz"),
                asset("checksums.txt"),
            ],
        };
        let available = AvailableRelease::from_github(
            release,
            ReleaseComponent::Cli,
            ReleasePlatform::LinuxX86_64,
        )
        .expect("matching release asset");
        assert_eq!(available.version, Version::parse("0.7.2").unwrap());
        assert_eq!(
            available.archive.name,
            "evo-cli-0.7.2-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(available.checksums.name, "checksums.txt");
    }

    #[test]
    fn release_selection_rejects_missing_or_unstable_releases() {
        let missing = GitHubRelease {
            tag_name: "v0.7.2".into(),
            draft: false,
            prerelease: false,
            assets: vec![asset("checksums.txt")],
        };
        assert!(matches!(
            AvailableRelease::from_github(
                missing,
                ReleaseComponent::Desktop,
                ReleasePlatform::WindowsX86_64,
            ),
            Err(UpdateError::MissingAsset(_))
        ));

        let prerelease = GitHubRelease {
            tag_name: "v0.8.0-beta.1".into(),
            draft: false,
            prerelease: true,
            assets: Vec::new(),
        };
        assert!(matches!(
            AvailableRelease::from_github(
                prerelease,
                ReleaseComponent::Cli,
                ReleasePlatform::LinuxX86_64,
            ),
            Err(UpdateError::NonStableRelease)
        ));
    }

    #[test]
    fn checksum_verification_rejects_missing_duplicate_invalid_and_mismatched_entries() {
        let name = "evo-cli-0.7.2-x86_64-unknown-linux-gnu.tar.gz";
        let bytes = b"release archive";
        let digest = hex_digest(bytes);
        assert_eq!(
            checksum_for(format!("{digest}  {name}\n").as_bytes(), name).unwrap(),
            digest
        );
        assert!(matches!(
            checksum_for(b"", name),
            Err(UpdateError::MissingChecksum { .. })
        ));
        assert!(matches!(
            checksum_for(format!("not-a-digest  {name}\n").as_bytes(), name),
            Err(UpdateError::InvalidChecksum { .. })
        ));
        assert!(matches!(
            checksum_for(
                format!("{digest}  {name}\n{digest}  {name}\n").as_bytes(),
                name
            ),
            Err(UpdateError::DuplicateChecksum { .. })
        ));

        let release = AvailableRelease {
            version: Version::parse("0.7.2").unwrap(),
            archive: ReleaseAsset {
                name: name.into(),
                download_url: "https://example.test/archive".into(),
                size: 1,
            },
            checksums: ReleaseAsset {
                name: CHECKSUMS_ASSET.into(),
                download_url: "https://example.test/checksums".into(),
                size: 1,
            },
        };
        let client = ReleaseClient::for_repository("test/repository").unwrap();
        assert!(
            client
                .verify_archive(&release, format!("{digest}  {name}\n").as_bytes(), bytes)
                .is_ok()
        );
        assert!(matches!(
            client.verify_archive(
                &release,
                format!("{digest}  {name}\n").as_bytes(),
                b"changed"
            ),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn semantic_release_comparison_uses_version_ordering() {
        let release = AvailableRelease {
            version: Version::parse("0.10.0").unwrap(),
            archive: ReleaseAsset {
                name: "archive".into(),
                download_url: "https://example.test/archive".into(),
                size: 1,
            },
            checksums: ReleaseAsset {
                name: CHECKSUMS_ASSET.into(),
                download_url: "https://example.test/checksums".into(),
                size: 1,
            },
        };
        assert!(release.is_newer_than(&Version::parse("0.9.9").unwrap()));
        assert!(!release.is_newer_than(&Version::parse("0.10.0").unwrap()));
    }
}
