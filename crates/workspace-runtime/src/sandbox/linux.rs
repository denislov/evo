//! Linux Landlock enforcement for [`SandboxProfile`].
//!
//! Landlock restricts the filesystem (and, from ABI v4 / kernel 6.7, TCP
//! connect/bind) of the calling process after a `fork`, before `exec`. The
//! ruleset is built in the parent (paths are canonicalized and rules added
//! there); the child only calls the two raw syscalls in
//! [`PreparedSandbox::restrict_self`], keeping the spawn boundary
//! async-signal-safe.
//!
//! The required ABI is fixed at compile time (`V3` for filesystem, `V4` for
//! network) instead of being derived from the running kernel, so behavior is
//! deterministic across kernels: a kernel below the requirement fails closed
//! with [`SandboxUnsupported`] instead of silently weakening the rules.

use std::error::Error;
use std::io::ErrorKind;
use std::os::unix::io::{AsRawFd, OwnedFd};

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr,
};

use super::{
    CapabilityDimension, NetworkPolicy, PreparedSandbox, SandboxCapability, SandboxProfile,
    SandboxUnsupported,
};

/// Filesystem rules require ABI v3 (REFER from v2, TRUNCATE from v3).
pub(super) const REQUIRED_FS_ABI: ABI = ABI::V3;
/// Network rules require ABI v4 (TCP connect/bind, kernel 6.7+).
pub(super) const REQUIRED_NET_ABI: ABI = ABI::V4;
const LANDLOCK_CREATE_RULESET_VERSION: usize = 1;

/// Probe the running kernel's Landlock ABI via the version syscall. Returns 0
/// when Landlock is not available (syscall missing or LSM not enabled).
pub(super) fn probe_abi() -> i32 {
    // SAFETY: pure probe syscall; a null attr pointer with the version flag
    // never allocates and has no side effects.
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        ) as i32
    }
}

pub(super) fn capability() -> SandboxCapability {
    let abi = probe_abi();
    let fs = if abi >= REQUIRED_FS_ABI as i32 {
        CapabilityDimension::supported(format!(
            "landlock abi={abi} (requires v{})",
            REQUIRED_FS_ABI as i32
        ))
    } else {
        CapabilityDimension::unsupported(format!(
            "linux landlock abi={abi} is below the required v{}",
            REQUIRED_FS_ABI as i32
        ))
    };
    let network = if abi >= REQUIRED_NET_ABI as i32 {
        CapabilityDimension::supported(format!(
            "landlock abi={abi} (requires v{})",
            REQUIRED_NET_ABI as i32
        ))
    } else {
        CapabilityDimension::unsupported(format!(
            "linux landlock abi={abi} is below the required v{}",
            REQUIRED_NET_ABI as i32
        ))
    };
    SandboxCapability {
        fs,
        network,
        exec: CapabilityDimension::supported("unrestricted only (fs-gated)"),
        env: CapabilityDimension::supported("spawn-time environment filter"),
    }
}

pub(super) fn prepare(
    profile: &SandboxProfile,
) -> Result<Option<PreparedSandbox>, SandboxUnsupported> {
    let need_fs = profile.constrains_fs();
    let need_net = profile.network != NetworkPolicy::All;
    if !need_fs && !need_net {
        return Ok(None);
    }
    let abi = probe_abi();
    if need_fs && abi < REQUIRED_FS_ABI as i32 {
        return Err(SandboxUnsupported::Dimension {
            dimension: "fs",
            reason: format!(
                "linux landlock abi={abi} is below the required v{}",
                REQUIRED_FS_ABI as i32
            ),
        });
    }
    if need_net && abi < REQUIRED_NET_ABI as i32 {
        return Err(SandboxUnsupported::Dimension {
            dimension: "network",
            reason: format!(
                "linux landlock abi={abi} is below the required v{} for tcp enforcement",
                REQUIRED_NET_ABI as i32
            ),
        });
    }

    let mut ruleset = Ruleset::default().set_compatibility(CompatLevel::HardRequirement);
    if need_fs {
        ruleset = ruleset
            .handle_access(AccessFs::from_all(REQUIRED_FS_ABI))
            .map_err(sandbox_apply_error)?;
    }
    if need_net {
        ruleset = ruleset
            .handle_access(AccessNet::from_all(REQUIRED_NET_ABI))
            .map_err(sandbox_apply_error)?;
    }
    let mut created = ruleset.create().map_err(sandbox_apply_error)?;

    for root in &profile.read_roots {
        created = add_fs_rule(created, root, AccessFs::from_read(REQUIRED_FS_ABI))?;
    }
    for root in &profile.write_roots {
        created = add_fs_rule(created, root, AccessFs::from_all(REQUIRED_FS_ABI))?;
    }
    // NetworkPolicy::None is enforced by handling the net accesses and adding
    // no port rule at all: Landlock denies every TCP connect/bind that has no
    // matching rule. Loopback is rejected in the platform-agnostic gate.

    let ruleset_fd: Option<OwnedFd> = created.into();
    let ruleset_fd = ruleset_fd.ok_or_else(|| SandboxUnsupported::Apply {
        reason: "landlock ruleset has no file descriptor".into(),
    })?;
    set_close_on_exec(&ruleset_fd)?;
    Ok(Some(PreparedSandbox { ruleset_fd }))
}

/// Add a `PathBeneath` rule for `root` with `access`. Missing paths are
/// skipped: Landlock denies them anyway (deny-by-default), so skipping is
/// never weaker than failing.
fn add_fs_rule(
    created: landlock::RulesetCreated,
    root: &std::path::Path,
    access: landlock::BitFlags<AccessFs>,
) -> Result<landlock::RulesetCreated, SandboxUnsupported> {
    match PathFd::new(root) {
        Ok(path) => created
            .add_rule(PathBeneath::new(path, access))
            .map_err(sandbox_apply_error),
        Err(error)
            if error.source().is_some_and(|source| {
                source
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == ErrorKind::NotFound)
            }) =>
        {
            Ok(created)
        }
        Err(error) => Err(SandboxUnsupported::Apply {
            reason: format!("cannot open sandbox root {}: {error}", root.display()),
        }),
    }
}

fn set_close_on_exec(fd: &OwnedFd) -> Result<(), SandboxUnsupported> {
    // SAFETY: `fd` is a valid descriptor owned by this structure; F_SETFD
    // never reads or writes user memory.
    let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    if rc != 0 {
        return Err(SandboxUnsupported::Apply {
            reason: format!(
                "cannot mark landlock ruleset descriptor close-on-exec: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    Ok(())
}

fn sandbox_apply_error(error: landlock::RulesetError) -> SandboxUnsupported {
    SandboxUnsupported::Apply {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests_linux;
