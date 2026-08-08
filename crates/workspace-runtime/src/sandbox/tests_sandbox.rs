//! Platform-agnostic sandbox unit tests: profile semantics, environment
//! resolution, and fail-closed validation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    CapabilityDimension, ExecPolicy, NetworkPolicy, SandboxCapability, SandboxProfile,
    SandboxUnsupported, prepare_sandbox, resolve_env,
};
use crate::process::EnvPolicy;

fn test_env() -> HashMap<String, String> {
    HashMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("HOME".into(), "/home/tester".into()),
        ("SECRET".into(), "leak".into()),
    ])
}

fn profile_with(read: Vec<&str>, write: Vec<&str>) -> SandboxProfile {
    SandboxProfile {
        read_roots: read.iter().map(PathBuf::from).collect(),
        write_roots: write.iter().map(PathBuf::from).collect(),
        exec: ExecPolicy::Unrestricted,
        network: NetworkPolicy::All,
        env: EnvPolicy::Inherit,
    }
}

#[test]
fn product_default_grants_workspace_system_and_home_reads() {
    let profile = SandboxProfile::product_default(Path::new("/workspace/evo"));
    assert!(
        profile
            .read_roots
            .contains(&PathBuf::from("/workspace/evo"))
    );
    for root in ["/bin", "/usr", "/etc", "/proc", "/tmp", "/nix"] {
        assert!(
            profile.read_roots.contains(&PathBuf::from(root)),
            "{root} readable"
        );
    }
    assert_eq!(
        profile.write_roots,
        vec![
            PathBuf::from("/workspace/evo"),
            PathBuf::from("/tmp"),
            PathBuf::from("/dev"),
        ]
    );
    assert_eq!(profile.exec, ExecPolicy::Unrestricted);
    assert_eq!(profile.network, NetworkPolicy::All);
    assert_eq!(profile.env, EnvPolicy::Inherit);
    assert!(profile.constrains_fs());
}

#[test]
fn empty_roots_do_not_constrain_fs() {
    let profile = profile_with(vec![], vec![]);
    assert!(!profile.constrains_fs());
    assert!(
        prepare_sandbox(&profile)
            .expect("no fs constraint requires no platform mechanism")
            .is_none()
    );
}

#[test]
fn resolve_env_keeps_spec_values_without_profile_constraint() {
    assert_eq!(
        resolve_env(&EnvPolicy::AllowList(test_env()), None),
        Some(test_env())
    );
    assert_eq!(resolve_env(&EnvPolicy::Inherit, None), None);
    assert_eq!(
        resolve_env(&EnvPolicy::Inherit, Some(&EnvPolicy::Inherit)),
        None
    );
}

#[test]
fn resolve_env_intersects_profile_constraint_with_spec_values() {
    let constraint = EnvPolicy::AllowList(HashMap::from([
        ("PATH".into(), String::new()),
        ("HOME".into(), String::new()),
    ]));
    let resolved = resolve_env(&EnvPolicy::AllowList(test_env()), Some(&constraint))
        .expect("allow-listed spec env resolves");
    assert_eq!(
        resolved,
        HashMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), "/home/tester".into()),
        ])
    );
    assert!(!resolved.contains_key("SECRET"));
}

#[test]
fn exec_allow_list_fails_closed_on_every_platform() {
    let profile = SandboxProfile {
        exec: ExecPolicy::AllowList(vec![PathBuf::from("/bin/sh")]),
        ..profile_with(vec!["/bin"], vec![])
    };
    let error = prepare_sandbox(&profile).expect_err("allow-list must be rejected");
    assert!(matches!(
        error,
        SandboxUnsupported::Dimension {
            dimension: "exec",
            ..
        }
    ));
    assert!(error.to_string().contains("exec"));
}

#[test]
fn loopback_network_fails_closed_on_every_platform() {
    let profile = SandboxProfile {
        network: NetworkPolicy::Loopback,
        ..profile_with(vec![], vec![])
    };
    let error = prepare_sandbox(&profile).expect_err("loopback must be rejected");
    assert!(matches!(
        error,
        SandboxUnsupported::Dimension {
            dimension: "network",
            ..
        }
    ));
}

#[test]
fn capability_dimension_carries_a_reason() {
    let supported = CapabilityDimension::supported("mechanism x");
    assert!(supported.supported);
    assert_eq!(supported.detail, "mechanism x");
    let unsupported = CapabilityDimension::unsupported("no mechanism");
    assert!(!unsupported.supported);
    assert_eq!(unsupported.detail, "no mechanism");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_capability_reports_env_and_exec_but_fs_mirrors_the_kernel() {
    use super::linux::probe_abi;
    let capability = SandboxCapability::current();
    assert!(capability.env.supported);
    assert!(capability.exec.supported);
    assert_eq!(
        capability.fs.supported,
        probe_abi() >= super::linux::REQUIRED_FS_ABI as i32
    );
    assert_eq!(
        capability.network.supported,
        probe_abi() >= super::linux::REQUIRED_NET_ABI as i32
    );
}
