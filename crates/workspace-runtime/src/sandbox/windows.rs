//! Windows sandbox tier: capability report + fail-closed.
//!
//! Job objects already contain the process tree, but restricted tokens /
//! AppContainer isolation for children is not implemented yet. The first
//! version reports the filesystem and network dimensions as unsupported.
//! Profiles that request them fail closed through [`SandboxUnsupported`];
//! callers may either refuse to run or grant an explicit, user-visible
//! degradation. There is no silent unrestricted path.

use super::{
    CapabilityDimension, NetworkPolicy, PreparedSandbox, SandboxCapability, SandboxProfile,
    SandboxUnsupported,
};

pub(super) fn capability() -> SandboxCapability {
    SandboxCapability {
        fs: CapabilityDimension::unsupported(
            "windows: restricted token / AppContainer enforcement not implemented",
        ),
        network: CapabilityDimension::unsupported("windows: network enforcement not implemented"),
        exec: CapabilityDimension::supported("unrestricted only"),
        env: CapabilityDimension::supported("spawn-time environment filter"),
    }
}

pub(super) fn prepare(
    profile: &SandboxProfile,
) -> Result<Option<PreparedSandbox>, SandboxUnsupported> {
    if profile.constrains_fs() {
        return Err(SandboxUnsupported::Platform {
            platform: "windows",
            reason: "filesystem enforcement is not implemented; refuse the profile or grant an explicit degradation"
                .into(),
        });
    }
    if profile.network != NetworkPolicy::All {
        return Err(SandboxUnsupported::Platform {
            platform: "windows",
            reason: "network enforcement is not implemented; refuse the profile or grant an explicit degradation"
                .into(),
        });
    }
    Ok(None)
}
