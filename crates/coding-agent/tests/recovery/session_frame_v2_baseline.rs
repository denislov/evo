//! Deliberately incompatible v2 durable-frame behavior.

use std::path::{Path, PathBuf};

use coding_agent::api::error::CodingAgentErrorCategory;
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/architecture-baseline-v1"
);

#[tokio::test]
async fn unframed_event_v2_without_session_sequence_is_rejected() {
    let fixture = Path::new(FIXTURES).join("session-v2-no-sequence");
    let (temp, session_dir) = copy_fixture(&fixture);
    let original = std::fs::read(session_dir.join("events.jsonl")).unwrap();

    let error = CodingAgentSession::open(
        CodingAgentSessionOptions::new()
            .with_session_log_root(temp.path())
            .with_session_path(&session_dir),
    )
    .await
    .expect_err("unframed pre-0.6.1 event records must be rejected");

    crate::support::assert_public_error(&error, CodingAgentErrorCategory::Session, "session", true);
    let serialized = serde_json::to_string(&error).unwrap();
    assert!(!serialized.contains("required v2 session event frame"));
    assert!(!serialized.contains("start a fresh 0.6.1 session store"));
    assert_eq!(
        std::fs::read(session_dir.join("events.jsonl")).unwrap(),
        original,
        "rejection must not rewrite the unsupported store"
    );
}

#[tokio::test]
async fn unframed_incomplete_event_v2_is_rejected_before_recovery() {
    let fixture = Path::new(FIXTURES).join("session-v2-incomplete");
    let (temp, session_dir) = copy_fixture(&fixture);
    let original = std::fs::read(session_dir.join("events.jsonl")).unwrap();
    let options = CodingAgentSessionOptions::new()
        .with_session_log_root(temp.path())
        .with_session_path(&session_dir);

    let error = CodingAgentSession::open(options)
        .await
        .expect_err("unsupported records must fail before startup recovery");

    crate::support::assert_public_error(&error, CodingAgentErrorCategory::Session, "session", true);
    let serialized = serde_json::to_string(&error).unwrap();
    assert!(!serialized.contains("required v2 session event frame"));
    assert!(!serialized.contains("start a fresh 0.6.1 session store"));
    assert_eq!(
        std::fs::read(session_dir.join("events.jsonl")).unwrap(),
        original,
        "failed recovery admission must not append a compatibility marker"
    );
}

fn copy_fixture(source: &Path) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("create fixture tempdir");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(source.join("session.json")).expect("read fixture manifest"),
    )
    .expect("fixture manifest should be valid JSON");
    let destination = temp.path().join(
        manifest["session_id"]
            .as_str()
            .expect("fixture manifest should contain a session id"),
    );
    std::fs::create_dir(&destination).expect("create copied fixture directory");
    for file in ["session.json", "events.jsonl"] {
        std::fs::copy(source.join(file), destination.join(file))
            .unwrap_or_else(|error| panic!("copy fixture file {file}: {error}"));
    }
    (temp, destination)
}
