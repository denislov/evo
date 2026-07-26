use std::fs;
use std::path::{Path, PathBuf};

const TEST_FILES_ALLOWED_TO_IMPORT_CORE_SESSION: &[&str] =
    &["crates/coding-agent/tests/recovery/session_boundary_guards.rs"];

#[test]
fn production_code_uses_transcript_boundary_for_core_session_types() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/coding-agent");
    let src_root = crate_root.join("src");
    let mut violations = Vec::new();

    collect_core_session_imports(repo_root, &src_root, &mut violations);

    assert!(
        violations.is_empty(),
        "coding-agent production code should import shared transcript/tree/id types through agent_core::transcript, not agent_core::session:\n{}",
        violations.join("\n")
    );
}

#[test]
fn transcript_only_tests_use_transcript_boundary_for_core_session_types() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/coding-agent");
    let tests_root = crate_root.join("tests");
    let mut violations = Vec::new();

    collect_core_session_imports_with_allowlist(
        repo_root,
        &tests_root,
        TEST_FILES_ALLOWED_TO_IMPORT_CORE_SESSION,
        &mut violations,
    );

    assert!(
        violations.is_empty(),
        "coding-agent tests should import shared transcript/tree/id types through agent_core::transcript, not agent_core::session:\n{}",
        violations.join("\n")
    );
}

fn collect_core_session_imports(repo_root: &Path, path: &Path, violations: &mut Vec<String>) {
    collect_core_session_imports_with_allowlist(repo_root, path, &[], violations);
}

fn collect_core_session_imports_with_allowlist(
    repo_root: &Path,
    path: &Path,
    allowed_files: &[&str],
    violations: &mut Vec<String>,
) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read source directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read source entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_core_session_imports_with_allowlist(
                repo_root,
                &entry.path(),
                allowed_files,
                violations,
            );
        }
        return;
    }

    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let relative = path
        .strip_prefix(repo_root)
        .expect("scanned file should be under repo root")
        .to_string_lossy()
        .replace('\\', "/");
    if allowed_files.contains(&relative.as_str()) {
        return;
    }

    let content = fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        if line.contains("agent_core::session") || line.contains("agent_core::api::session") {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}
