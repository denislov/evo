use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED_SESSION_IMPORT_FILES: &[&str] = &[];

const TEST_FILES_ALLOWED_TO_IMPORT_SESSION: &[&str] =
    &["crates/agent-core/tests/api_contract/transcript_boundary.rs"];

const TRANSCRIPT_SYMBOLS: &[&str] = &[
    "SessionEntry",
    "SessionHeader",
    "SessionMetadata",
    "SessionTreeNode",
    "StoredAgentMessage",
    "StoredUsage",
    "StoredUsageCost",
    "SessionIdGenerator",
    "create_session_id",
    "create_timestamp",
    "generate_entry_id",
    "agent_message_to_stored",
];

#[test]
fn transcript_module_owns_shared_transcript_sources() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let transcript_root = crate_root.join("src/transcript");
    let transcript_mod = fs::read_to_string(transcript_root.join("mod.rs"))
        .expect("transcript module should be readable");

    assert!(
        transcript_root.join("id.rs").is_file(),
        "shared transcript id helpers should live under src/transcript/id.rs"
    );
    assert!(
        transcript_root.join("types.rs").is_file(),
        "shared transcript/tree/message types should live under src/transcript/types.rs"
    );
    for forbidden in [
        "crate::session::id",
        "crate::session::types",
        "crate::session::agent_message_to_stored",
    ] {
        assert!(
            !transcript_mod.contains(forbidden),
            "transcript module should own shared sources instead of re-exporting {forbidden}"
        );
    }
    for relative in [
        "src/transcript/mod.rs",
        "src/transcript/id.rs",
        "src/transcript/types.rs",
    ] {
        let source = fs::read_to_string(crate_root.join(relative))
            .unwrap_or_else(|error| panic!("{relative} should be readable: {error}"));
        assert!(
            !source.contains("crate::session"),
            "transcript source should not depend on session compatibility module: {relative}"
        );
    }

    assert!(
        !crate_root.join("src/session").exists(),
        "legacy session compatibility module should be removed after transcript/context own the real sources"
    );
}

#[test]
fn transcript_only_tests_use_neutral_transcript_boundary() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/agent-core");
    let tests_root = crate_root.join("tests");
    let mut violations = Vec::new();

    collect_public_session_imports(
        repo_root,
        &tests_root,
        TEST_FILES_ALLOWED_TO_IMPORT_SESSION,
        &mut violations,
    );

    assert!(
        violations.is_empty(),
        "agent-core tests should use agent_core::transcript for transcript/tree/id symbols and agent_core::context for session-context behavior; do not import agent_core::session:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_transcript_symbols_use_neutral_transcript_boundary() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/agent-core");
    let src_root = crate_root.join("src");
    let mut violations = Vec::new();

    collect_session_transcript_imports(repo_root, &src_root, &mut violations);

    assert!(
        violations.is_empty(),
        "production code should import transcript/tree/id symbols through crate::transcript, not crate::session:\n{}",
        violations.join("\n")
    );
}

fn collect_public_session_imports(
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
            collect_public_session_imports(repo_root, &entry.path(), allowed_files, violations);
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
        if line.contains("agent_core::session::")
            || line.contains("use agent_core::session::{")
            || line.contains("agent_core::api::session::")
        {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}

fn collect_session_transcript_imports(repo_root: &Path, path: &Path, violations: &mut Vec<String>) {
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
            collect_session_transcript_imports(repo_root, &entry.path(), violations);
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
    if ALLOWED_SESSION_IMPORT_FILES.contains(&relative.as_str()) {
        return;
    }

    let content = fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        if !line.contains("crate::session") {
            continue;
        }
        if TRANSCRIPT_SYMBOLS
            .iter()
            .any(|symbol| line.contains(symbol))
        {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}
