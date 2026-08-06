//! Linux Landlock integration tests: run real child processes through the
//! spawn boundary and verify the OS policy actually holds.
//!
//! Every test probes `SandboxCapability` first and skips (with an explicit
//! message) when the running kernel cannot enforce the dimension. Production
//! paths still fail closed; only the tests skip.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{NetworkPolicy, PreparedSandbox, REQUIRED_FS_ABI, REQUIRED_NET_ABI, probe_abi};
use crate::process::{EnvPolicy, OutputBudget, ProcessOutcome, ProcessSpec, ProgramKind, run};
use crate::sandbox::{ExecPolicy, SandboxProfile};

fn fs_enforced() -> bool {
    let enforced = probe_abi() >= REQUIRED_FS_ABI as i32;
    if !enforced {
        eprintln!(
            "SKIP: kernel landlock abi={} is below the required v{}",
            probe_abi(),
            REQUIRED_FS_ABI as i32
        );
    }
    enforced
}

fn net_enforced() -> bool {
    let enforced = probe_abi() >= REQUIRED_NET_ABI as i32;
    if !enforced {
        eprintln!(
            "SKIP: kernel landlock abi={} is below the required v{} for network",
            probe_abi(),
            REQUIRED_NET_ABI as i32
        );
    }
    enforced
}

/// Minimal profile: the shell itself plus system directories are readable,
/// `tmp` holds the tempdirs this test writes into. `/tmp` itself stays out of
/// the roots so sibling tempdirs remain unauthorized.
fn minimal_profile(
    read_extra: Vec<std::path::PathBuf>,
    write_roots: Vec<std::path::PathBuf>,
) -> SandboxProfile {
    let mut read_roots = vec![
        std::path::PathBuf::from("/bin"),
        std::path::PathBuf::from("/sbin"),
        std::path::PathBuf::from("/usr"),
        std::path::PathBuf::from("/lib"),
        std::path::PathBuf::from("/lib64"),
        std::path::PathBuf::from("/etc"),
        std::path::PathBuf::from("/proc"),
        std::path::PathBuf::from("/dev"),
        std::path::PathBuf::from("/run"),
    ];
    read_roots.extend(read_extra);
    SandboxProfile {
        read_roots,
        write_roots,
        exec: ExecPolicy::Unrestricted,
        network: NetworkPolicy::All,
        env: EnvPolicy::Inherit,
    }
}

async fn run_sh(cwd: &Path, sandbox: Option<SandboxProfile>, script: &str) -> ProcessOutcome {
    let spec = ProcessSpec {
        program: ProgramKind::Shell {
            path: "/bin/sh".into(),
            command_arg: "-c".into(),
        },
        command: script.into(),
        cwd: cwd.to_path_buf(),
        env: EnvPolicy::AllowList(HashMap::from([("PATH".into(), "/usr/bin:/bin".into())])),
        timeout: Duration::from_secs(10),
        output_budget: OutputBudget::new(64 * 1024, 2000),
        sandbox,
    };
    run(spec, &CancellationToken::new(), None).await
}

fn merged(outcome: &ProcessOutcome) -> String {
    match outcome {
        ProcessOutcome::Completed { output, .. }
        | ProcessOutcome::TimedOut { output }
        | ProcessOutcome::Cancelled { output }
        | ProcessOutcome::Failed { output, .. } => output.merged.clone(),
    }
}

#[tokio::test]
async fn write_outside_write_roots_is_rejected_by_landlock() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    let target = victim.path().join("escaped.txt");
    let outcome = run_sh(
        temp.path(),
        Some(minimal_profile(vec![temp.path().to_path_buf()], vec![])),
        &format!("echo x > {}", target.display()),
    )
    .await;
    let ProcessOutcome::Completed { exit_code, .. } = &outcome else {
        panic!("expected a completed process: {:?}", outcome);
    };
    assert_ne!(
        *exit_code,
        Some(0),
        "write outside roots must fail: {}",
        merged(&outcome)
    );
    assert!(
        merged(&outcome).contains("Permission denied"),
        "expected EACCES in output: {}",
        merged(&outcome)
    );
    assert!(!target.exists(), "the file must not be created");
}

#[tokio::test]
async fn read_outside_read_roots_is_rejected_by_landlock() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    let secret = victim.path().join("secret.txt");
    std::fs::write(&secret, "classified").unwrap();
    let outcome = run_sh(
        temp.path(),
        Some(minimal_profile(vec![temp.path().to_path_buf()], vec![])),
        &format!("cat {}", secret.display()),
    )
    .await;
    let ProcessOutcome::Completed { exit_code, .. } = &outcome else {
        panic!("expected a completed process: {:?}", outcome);
    };
    assert_ne!(
        *exit_code,
        Some(0),
        "read outside roots must fail: {}",
        merged(&outcome)
    );
}

#[tokio::test]
async fn write_inside_write_root_succeeds() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("output.txt");
    let outcome = run_sh(
        temp.path(),
        Some(minimal_profile(vec![], vec![temp.path().to_path_buf()])),
        &format!("echo hello > {}", target.display()),
    )
    .await;
    assert!(
        matches!(
            outcome,
            ProcessOutcome::Completed {
                exit_code: Some(0),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello\n");
}

#[tokio::test]
async fn read_inside_read_root_succeeds() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("data.txt");
    std::fs::write(&file, "visible").unwrap();
    let outcome = run_sh(
        temp.path(),
        Some(minimal_profile(vec![temp.path().to_path_buf()], vec![])),
        &format!("cat {}", file.display()),
    )
    .await;
    let ProcessOutcome::Completed { exit_code, output } = &outcome else {
        panic!("expected a completed process: {:?}", outcome);
    };
    assert_eq!(
        *exit_code,
        Some(0),
        "read inside roots must succeed: {}",
        merged(&outcome)
    );
    assert!(output.merged.contains("visible"));
}

#[tokio::test]
async fn write_to_read_only_authorized_root_fails() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let outcome = run_sh(
        temp.path(),
        Some(minimal_profile(vec![temp.path().to_path_buf()], vec![])),
        &format!("echo x > {}/denied.txt", temp.path().display()),
    )
    .await;
    let ProcessOutcome::Completed { exit_code, .. } = &outcome else {
        panic!("expected a completed process: {:?}", outcome);
    };
    assert_ne!(
        *exit_code,
        Some(0),
        "read-only root must reject writes: {}",
        merged(&outcome)
    );
}

#[tokio::test]
async fn no_sandbox_preserves_legacy_unrestricted_behavior() {
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let outcome = run_sh(temp.path(), None, "echo legacy-ok").await;
    assert!(
        matches!(
            outcome,
            ProcessOutcome::Completed {
                exit_code: Some(0),
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn network_none_rejects_tcp_connect() {
    if !net_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let mut profile = minimal_profile(
        vec![temp.path().to_path_buf()],
        vec![temp.path().to_path_buf()],
    );
    profile.network = NetworkPolicy::None;
    let outcome = run_sh(
        temp.path(),
        Some(profile),
        "exec 3<>/dev/tcp/127.0.0.1/9; echo connected",
    )
    .await;
    let ProcessOutcome::Completed { exit_code, .. } = &outcome else {
        panic!("expected a completed process: {:?}", outcome);
    };
    assert_ne!(
        *exit_code,
        Some(0),
        "network=none must reject connect: {}",
        merged(&outcome)
    );
}

#[test]
fn prepared_sandbox_restrict_self_is_async_signal_safe_shaped() {
    // The prepared sandbox owns a ruleset descriptor and the only child-side
    // method performs raw syscalls; this test exercises the prepare path on
    // supported kernels so the pre_exec closure is at least exercised once
    // outside of spawn.
    if !fs_enforced() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let profile = minimal_profile(
        vec![temp.path().to_path_buf()],
        vec![temp.path().to_path_buf()],
    );
    let prepared: Option<PreparedSandbox> =
        crate::sandbox::prepare_sandbox(&profile).expect("supported kernel prepares");
    assert!(prepared.is_some());
}
