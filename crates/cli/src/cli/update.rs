use semver::Version;

use release_updater::{InstallOutcome, ReleaseClient, ReleaseComponent, ReleasePlatform};

const UPDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const STARTUP_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Returns a user-facing notice when a newer stable GitHub Release exists.
///
/// A failed or slow check is intentionally invisible: update availability must
/// never make a normal CLI invocation fail.
pub(crate) async fn startup_notice() -> Option<String> {
    let platform = ReleasePlatform::current().ok()?;
    let client = ReleaseClient::new().ok()?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    let available = tokio::time::timeout(
        STARTUP_CHECK_TIMEOUT,
        client.latest(ReleaseComponent::Cli, platform),
    )
    .await
    .ok()?
    .ok()?;
    available.is_newer_than(&current).then(|| {
        format!(
            "A newer coding-agent version ({}) is available. Run `coding-agent update` to install it.\n",
            available.version
        )
    })
}

pub(crate) async fn install_latest() -> Result<String, String> {
    let platform = ReleasePlatform::current().map_err(|error| error.to_string())?;
    let client = ReleaseClient::new().map_err(|error| error.to_string())?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("invalid compiled CLI version: {error}"))?;
    let available = tokio::time::timeout(
        UPDATE_TIMEOUT,
        client.latest(ReleaseComponent::Cli, platform),
    )
    .await
    .map_err(|_| "timed out while checking GitHub Releases".to_string())?
    .map_err(|error| error.to_string())?;
    if !available.is_newer_than(&current) {
        return Ok(format!("coding-agent {current} is already up to date\n"));
    }

    let target = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running executable: {error}"))?;
    let staged = tokio::time::timeout(
        UPDATE_TIMEOUT,
        client.stage_latest_for(ReleaseComponent::Cli, platform, &target),
    )
    .await
    .map_err(|_| "timed out while downloading the GitHub Release".to_string())?
    .map_err(|error| error.to_string())?;
    match staged
        .install_over(&target)
        .map_err(|error| error.to_string())?
    {
        InstallOutcome::Replaced => Ok(format!(
            "Updated coding-agent from {current} to {}. Restart coding-agent to use the new version.\n",
            staged.version()
        )),
        InstallOutcome::WindowsReplacementScheduled { helper } => Ok(format!(
            "Prepared coding-agent {}. Close this process; Windows will finish replacement via {}.\n",
            staged.version(),
            helper.display()
        )),
    }
}
