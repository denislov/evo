//! Unsupported platform tier: capability report + fail-closed.

use super::{
    CapabilityDimension, NetworkPolicy, PreparedSandbox, SandboxCapability, SandboxProfile,
    SandboxUnsupported,
};

pub(super) fn capability() -> SandboxCapability {
    SandboxCapability {
        fs: CapabilityDimension::unsupported("unsupported platform: no sandbox mechanism"),
        network: CapabilityDimension::unsupported("unsupported platform: no sandbox mechanism"),
        exec: CapabilityDimension::supported("unrestricted only"),
        env: CapabilityDimension::supported("spawn-time environment filter"),
    }
}

pub(super) fn prepare(
    profile: &SandboxProfile,
) -> Result<Option<PreparedSandbox>, SandboxUnsupported> {
    if profile.constrains_fs() {
        return Err(SandboxUnsupported::Platform {
            platform: "unsupported",
            reason: "no filesystem enforcement mechanism exists on this platform".into(),
        });
    }
    if profile.network != NetworkPolicy::All {
        return Err(SandboxUnsupported::Platform {
            platform: "unsupported",
            reason: "no network enforcement mechanism exists on this platform".into(),
        });
    }
    Ok(None)
}
