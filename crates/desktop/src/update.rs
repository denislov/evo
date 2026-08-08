//! Desktop-specific update orchestration over the shared GitHub Release protocol.

#[cfg(not(test))]
use semver::Version;

use release_updater::{InstallOutcome, ReleaseClient, ReleaseComponent, ReleasePlatform};

#[cfg(not(test))]
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[cfg(not(test))]
pub(crate) async fn check_for_update() -> Option<String> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("evo-update-check".into())
        .spawn(move || {
            let _ = sender.send(check_for_update_on_tokio_runtime());
        })
        .ok()?;
    receiver.await.ok().flatten()
}

#[cfg(not(test))]
fn check_for_update_on_tokio_runtime() -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let platform = ReleasePlatform::current().ok()?;
        let client = ReleaseClient::new().ok()?;
        let current = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
        let available = tokio::time::timeout(
            CHECK_TIMEOUT,
            client.latest(ReleaseComponent::Desktop, platform),
        )
        .await
        .ok()?
        .ok()?;
        available
            .is_newer_than(&current)
            .then(|| available.version.to_string())
    })
}

pub(crate) async fn install_latest() -> Result<String, String> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("evo-update-install".into())
        .spawn(move || {
            let _ = sender.send(install_latest_on_tokio_runtime());
        })
        .map_err(|error| format!("failed to start the update worker: {error}"))?;
    receiver
        .await
        .map_err(|_| "the update worker stopped unexpectedly".to_string())?
}

fn install_latest_on_tokio_runtime() -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to initialize the update worker: {error}"))?;
    runtime.block_on(async {
        let platform = ReleasePlatform::current().map_err(|error| error.to_string())?;
        let client = ReleaseClient::new().map_err(|error| error.to_string())?;
        let target = std::env::current_exe()
            .map_err(|error| format!("cannot locate the running executable: {error}"))?;
        let staged = tokio::time::timeout(
            INSTALL_TIMEOUT,
            client.stage_latest_for(ReleaseComponent::Desktop, platform, &target),
        )
        .await
        .map_err(|_| "timed out while downloading the GitHub Release".to_string())?
        .map_err(|error| error.to_string())?;
        let version = staged.version().to_string();
        match staged
            .install_over(&target)
            .map_err(|error| error.to_string())?
        {
            InstallOutcome::Replaced => Ok(format!(
                "Evo {version} is installed. Restart Evo to use the new version."
            )),
            InstallOutcome::WindowsReplacementScheduled { .. } => Ok(format!(
                "Evo {version} is ready. Close Evo to finish updating."
            )),
        }
    })
}
