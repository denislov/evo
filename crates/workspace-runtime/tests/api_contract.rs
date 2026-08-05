use std::fs;
use std::path::PathBuf;

#[test]
fn stable_facade_is_the_only_public_root_module() {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read workspace-runtime lib.rs");
    let public_modules = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .map(|module| module.trim_end_matches(';'))
        .collect::<Vec<_>>();
    assert_eq!(public_modules, ["api"]);
}

#[test]
fn facade_exposes_only_the_opaque_workspace_authority_handle() {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/api.rs"))
        .expect("read workspace-runtime api facade");
    assert!(source.contains("WorkspaceAccessHandle"));
    assert!(!source.contains("FilesystemCapability"));
    assert!(!source.contains("ShellCapability"));
}

#[test]
fn managed_worktree_creation_couples_identity_and_materialization_report() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let facade = fs::read_to_string(root.join("src/api.rs")).expect("read api facade");
    let worktree = fs::read_to_string(root.join("src/worktree.rs")).expect("read worktree module");
    assert!(facade.contains("ManagedWorktree"));
    assert!(worktree.contains("source: WorkspaceHandle"));
    assert!(worktree.contains("lease: WorkspaceLease"));
    assert!(worktree.contains("report: WorktreeReport"));
    assert!(worktree.contains("Result<ManagedWorktree, WorktreeError>"));
}
