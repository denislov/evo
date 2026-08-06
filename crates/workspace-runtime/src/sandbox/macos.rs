//! macOS sandbox tier: capability report + fail-closed.
//!
//! Seatbelt (`sandbox-exec`) is deprecated and unreliable on modern macOS, and
//! no App Sandbox container is attached to child processes here yet. The
//! first version therefore reports the filesystem and network dimensions as
//! unsupported. Profiles that request them fail closed through
//! [`SandboxUnsupported`]; callers may either refuse to run or grant an
//! explicit, user-visible degradation. There is no silent unrestricted path.

use super::{
    CapabilityDimension, NetworkPolicy, PreparedSandbox, SandboxCapability, SandboxProfile,
    SandboxUnsupported,
};

pub(super) fn capability() -> SandboxCapability {
    SandboxCapability {
        fs: CapabilityDimension::unsupported(
            "macos: seatbelt/sandbox-exec enforcement not implemented",
        ),
        network: CapabilityDimension::unsupported("macos: network enforcement not implemented"),
        exec: CapabilityDimension::supported("unrestricted only"),
        env: CapabilityDimension::supported("spawn-time environment filter"),
    }
}

pub(super) fn prepare(
    profile: &SandboxProfile,
) -> Result<Option<PreparedSandbox>, SandboxUnsupported> {
    if profile.constrains_fs() {
        return Err(SandboxUnsupported::Platform {
            platform: "macos",
            reason: "filesystem enforcement is not implemented; refuse the profile or grant an explicit degradation"
                .into(),
        });
    }
    if profile.network != NetworkPolicy::All {
        return Err(SandboxUnsupported::Platform {
            platform: "macos",
            reason: "network enforcement is not implemented; refuse the profile or grant an explicit degradation"
                .into(),
        });
    }
    Ok(None)
}
