//! Child-process sandbox profiles enforced at the spawn boundary.
//!
//! A [`SandboxProfile`] is a platform-agnostic description of what a spawned
//! child is allowed to do. It is prepared before spawn and enforced in the
//! child (`pre_exec` on Unix), never on the Desktop main process. Platforms
//! that cannot enforce a requested dimension fail with an explicit
//! [`SandboxUnsupported`] error; callers decide between fail-closed and an
//! explicit degradation grant. There is no silent unrestricted fallback.
//!
//! Dimension semantics:
//! - `read_roots` / `write_roots`: explicit allow-lists of absolute paths. An
//!   empty list means "no constraint on that direction" (inherits the host),
//!   matching the "explicit collection vs inherited default" contract. A
//!   non-empty list denies everything else (fail-closed per path).
//! - `exec`: [`ExecPolicy::Unrestricted`] (the only supported value today) or
//!   an allow-list (reserved; rejected everywhere until an enforcement
//!   mechanism exists).
//! - `network`: [`NetworkPolicy::None`] (deny all TCP), `All` (no constraint)
//!   or `Loopback` (reserved; rejected everywhere until implemented).
//! - `env`: reuses [`EnvPolicy`] as a process-level constraint applied on top
//!   of `ProcessSpec.env` (which carries the actual values). `AllowList`
//!   restricts the visible environment to the listed keys; `Inherit` leaves
//!   the spec-level policy untouched.
//!
//! See `docs/refactor/phase6-child-sandbox.md` for the platform capability
//! tiers and the product default policy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::process::EnvPolicy;

/// What a spawned child may do with the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Deny all TCP traffic (no socket may be opened).
    None,
    /// Allow loopback TCP only. Reserved: no platform enforces it yet, and
    /// profiles requesting it fail closed.
    Loopback,
    /// No network constraint.
    All,
}

/// What a spawned child may execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecPolicy {
    /// No execve restriction beyond what the filesystem grants.
    Unrestricted,
    /// Explicit allow-list of executable paths. Reserved: no platform
    /// enforces it yet, and profiles requesting it fail closed.
    AllowList(Vec<PathBuf>),
}

/// Platform-agnostic sandbox description applied at the child spawn boundary.
///
/// Empty `read_roots` / `write_roots` mean "no constraint on that direction",
/// so an empty profile still denies nothing. Non-empty lists are exclusive:
/// paths outside the listed roots are denied by the OS policy (on platforms
/// that enforce it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProfile {
    /// Absolute paths the child may read (and execute from).
    pub read_roots: Vec<PathBuf>,
    /// Absolute paths the child may modify. Write roots also grant read
    /// access (creation, removal, rename, truncate included).
    pub write_roots: Vec<PathBuf>,
    /// Executable program policy.
    pub exec: ExecPolicy,
    /// Network policy.
    pub network: NetworkPolicy,
    /// Process-level environment constraint applied on top of
    /// `ProcessSpec.env`.
    pub env: EnvPolicy,
}

/// System directories a product shell needs to read/execute from. Kept
/// explicit instead of granting `/` so that paths outside these roots stay
/// denied. `/nix` covers NixOS-only package storage: home-manager user
/// configs (e.g. `~/.config/git/config`) are symlinks into `/nix/store`, and
/// git follows them when reading config; on non-Nix systems the path does not
/// exist and the rule is skipped without weakening anything.
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc", "/opt", "/var", "/proc", "/dev", "/run",
    "/tmp", "/nix",
];

impl SandboxProfile {
    /// The product default profile for a shell granted on `workspace_root`:
    /// read the workspace and system directories, write only inside the
    /// workspace, `/tmp` and `/dev` (for `/dev/null`-style sinks), unrestricted
    /// exec (still gated by the filesystem rights), unrestricted network, and
    /// no extra environment constraint (the shell tool already passes an
    /// allow-listed environment through `ProcessSpec.env`).
    pub fn product_default(workspace_root: &Path) -> Self {
        let mut read_roots: Vec<PathBuf> = SYSTEM_READ_ROOTS.iter().map(PathBuf::from).collect();
        if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
            read_roots.push(PathBuf::from(home));
        }
        read_roots.push(workspace_root.to_path_buf());
        Self {
            read_roots,
            write_roots: vec![
                workspace_root.to_path_buf(),
                PathBuf::from("/tmp"),
                PathBuf::from("/dev"),
            ],
            exec: ExecPolicy::Unrestricted,
            network: NetworkPolicy::All,
            env: EnvPolicy::Inherit,
        }
    }

    /// Whether this profile requests filesystem constraints.
    pub fn constrains_fs(&self) -> bool {
        !self.read_roots.is_empty() || !self.write_roots.is_empty()
    }
}

/// One dimension of platform support. `supported == false` carries the
/// machine-readable reason in `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDimension {
    pub supported: bool,
    pub detail: String,
}

impl CapabilityDimension {
    fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            detail: reason.into(),
        }
    }

    fn supported(detail: impl Into<String>) -> Self {
        Self {
            supported: true,
            detail: detail.into(),
        }
    }
}

/// Report of what the current platform actually enforces per dimension.
///
/// This is the fail-closed gate: callers must probe it before applying a
/// profile and treat an unsupported dimension as an explicit error, never as
/// permission to run unrestricted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCapability {
    pub fs: CapabilityDimension,
    pub network: CapabilityDimension,
    pub exec: CapabilityDimension,
    pub env: CapabilityDimension,
}

impl SandboxCapability {
    /// Probe the current platform. Cheap and side-effect free.
    pub fn current() -> Self {
        platform::capability()
    }

    pub fn fs_supported(&self) -> bool {
        self.fs.supported
    }

    pub fn network_supported(&self) -> bool {
        self.network.supported
    }
}

/// Why a sandbox profile cannot be applied. Every unsupported path is an
/// explicit error; there is no silent downgrade to unrestricted execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxUnsupported {
    #[error("sandbox is not supported on {platform}: {reason}")]
    Platform {
        platform: &'static str,
        reason: String,
    },
    #[error("sandbox dimension `{dimension}` is not supported: {reason}")]
    Dimension {
        dimension: &'static str,
        reason: String,
    },
    #[error("sandbox profile cannot be applied: {reason}")]
    Apply { reason: String },
}

/// A prepared sandbox bound to the spawn boundary. The platform pieces are
/// built in the parent before spawn; only [`PreparedSandbox::restrict_self`]
/// runs in the child (`pre_exec`) and must therefore be async-signal-safe.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct PreparedSandbox {
    ruleset_fd: std::os::unix::io::OwnedFd,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct PreparedSandbox {
    _private: (),
}

impl PreparedSandbox {
    /// Enforce the sandbox in the current (child) process. Must only be
    /// called between `fork` and `exec`; it allocates nothing, panics on
    /// nothing, and performs only raw syscalls.
    #[cfg(target_os = "linux")]
    pub(crate) fn restrict_self(&self) -> std::io::Result<()> {
        // SAFETY: PR_SET_NO_NEW_PRIVS is a pure prctl with two integer
        // arguments; failing to set it must abort the child, not proceed
        // with a possibly privileged process.
        let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `ruleset_fd` is a valid landlock ruleset descriptor created
        // in the parent, flags must be 0, and landlock_restrict_self is a
        // plain syscall (async-signal-safe). The fd is close-on-exec and
        // unused after the child is gone.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_landlock_restrict_self,
                self.ruleset_fd.as_raw_fd(),
                0usize,
            )
        };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn restrict_self(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

/// Validate the platform-independent dimensions and dispatch to the platform
/// implementation. `None` means the profile needs no OS-level enforcement
/// (environment constraints are applied at spawn time by the caller).
pub(crate) fn prepare_sandbox(
    profile: &SandboxProfile,
) -> Result<Option<PreparedSandbox>, SandboxUnsupported> {
    if !matches!(profile.exec, ExecPolicy::Unrestricted) {
        return Err(SandboxUnsupported::Dimension {
            dimension: "exec",
            reason:
                "exec allow-list has no enforcement mechanism yet; use ExecPolicy::Unrestricted"
                    .into(),
        });
    }
    if profile.network == NetworkPolicy::Loopback {
        return Err(SandboxUnsupported::Dimension {
            dimension: "network",
            reason: "loopback policy has no enforcement mechanism yet; use none or all".into(),
        });
    }
    platform::prepare(profile)
}

/// The environment a child actually sees, after combining `ProcessSpec.env`
/// (the values) with the profile's environment constraint (the process-level
/// allow-list). `None` means the environment is inherited untouched.
pub(crate) fn resolve_env(
    spec_env: &EnvPolicy,
    profile_env: Option<&EnvPolicy>,
) -> Option<HashMap<String, String>> {
    let values = match spec_env {
        EnvPolicy::Inherit => None,
        EnvPolicy::AllowList(values) => Some(values.clone()),
    };
    match profile_env {
        None | Some(EnvPolicy::Inherit) => values,
        Some(EnvPolicy::AllowList(keys)) => {
            let filtered = match values {
                Some(values) => values
                    .into_iter()
                    .filter(|(key, _)| keys.contains_key(key))
                    .collect::<HashMap<_, _>>(),
                None => std::env::vars()
                    .filter(|(key, _)| keys.contains_key(key))
                    .collect::<HashMap<_, _>>(),
            };
            Some(filtered)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod other;

#[cfg(target_os = "linux")]
mod platform {
    pub(super) use super::linux::{capability, prepare};
}

#[cfg(target_os = "macos")]
mod platform {
    pub(super) use super::macos::{capability, prepare};
}

#[cfg(windows)]
mod platform {
    pub(super) use super::windows::{capability, prepare};
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    pub(super) use super::other::{capability, prepare};
}

#[cfg(test)]
mod tests_sandbox;
