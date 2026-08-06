//! identity 三要素与 `ParserVersion` 的测试。

use crate::{CacheIdentity, IdentityDiff, ParserVersion, RevisionId};
use workspace_runtime::api::{WorkspaceId, WorkspaceKind};

fn identity(workspace: &str, revision: &str, parser: ParserVersion) -> CacheIdentity {
    CacheIdentity {
        workspace: WorkspaceId::parse(workspace).unwrap(),
        revision: RevisionId::parse(revision).unwrap(),
        parser_version: parser,
    }
}

#[test]
fn parser_version_legacy_always_needs_rebuild() {
    assert!(ParserVersion::Legacy.needs_rebuild(0));
    assert!(ParserVersion::Legacy.needs_rebuild(42));
    assert!(ParserVersion::Legacy.needs_rebuild(u64::MAX));
}

#[test]
fn parser_version_matches_current() {
    assert!(!ParserVersion::Version(42).needs_rebuild(42));
}

#[test]
fn parser_version_mismatch_needs_rebuild() {
    assert!(ParserVersion::Version(42).needs_rebuild(43));
    assert!(ParserVersion::Version(0).needs_rebuild(1));
}

#[test]
fn parser_version_round_trip_json() {
    let version = ParserVersion::Version(0xDEAD_BEEF);
    let json = serde_json::to_value(version).unwrap();
    assert_eq!(json, serde_json::json!({"Version": 0xDEAD_BEEF_u64}));
    let back: ParserVersion = serde_json::from_value(json).unwrap();
    assert_eq!(back, version);

    let legacy = ParserVersion::Legacy;
    let json = serde_json::to_value(legacy).unwrap();
    assert_eq!(json, serde_json::json!("Legacy"));
    let back: ParserVersion = serde_json::from_value(json).unwrap();
    assert_eq!(back, legacy);
}

#[test]
fn revision_parse_accepts_printable_ascii() {
    assert_eq!(RevisionId::parse("HEAD~1").unwrap().as_str(), "HEAD~1");
    assert_eq!(
        RevisionId::parse("main@2026-08-07T10:00:00Z")
            .unwrap()
            .as_str(),
        "main@2026-08-07T10:00:00Z"
    );
    assert_eq!(RevisionId::parse("v 1.0").unwrap().as_str(), "v 1.0");
}

#[test]
fn revision_parse_rejects_invalid() {
    assert!(RevisionId::parse("").is_err());
    assert!(RevisionId::parse("has\tcontrol").is_err());
    assert!(RevisionId::parse("has\nnewline").is_err());
    assert!(RevisionId::parse("非ascii").is_err());
    let long = "x".repeat(129);
    assert!(RevisionId::parse(long).is_err());
    let ok = "x".repeat(128);
    assert!(RevisionId::parse(ok).is_ok());
}

#[test]
fn revision_round_trip_json() {
    let revision = RevisionId::parse("git-abc123").unwrap();
    let json = serde_json::to_value(&revision).unwrap();
    assert_eq!(json, serde_json::json!("git-abc123"));
    let back: RevisionId = serde_json::from_value(json).unwrap();
    assert_eq!(back, revision);
    // 非法字符串反序列化失败（fail closed）。
    assert!(serde_json::from_value::<RevisionId>(serde_json::json!("")).is_err());
}

#[test]
fn cache_identity_golden_json() {
    let id = CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "demo").unwrap(),
        revision: RevisionId::parse("rev-1").unwrap(),
        parser_version: ParserVersion::Version(7),
    };
    let json = serde_json::to_value(&id).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "workspace": "source-demo",
            "revision": "rev-1",
            "parser_version": {"Version": 7}
        })
    );
    let back: CacheIdentity = serde_json::from_value(json).unwrap();
    assert_eq!(back, id);
}

#[test]
fn cache_identity_round_trip_workspace_serde() {
    let id = identity("source-abc", "r1", ParserVersion::Legacy);
    let json = serde_json::to_value(&id).unwrap();
    let back: CacheIdentity = serde_json::from_value(json).unwrap();
    assert_eq!(back, id);
    // 非法 WorkspaceId 反序列化失败（fail closed）。
    assert!(
        serde_json::from_value::<CacheIdentity>(serde_json::json!({
            "workspace": "not-a-valid-prefix",
            "revision": "r1",
            "parser_version": "Legacy"
        }))
        .is_err()
    );
}

#[test]
fn mismatch_detects_each_element_independently() {
    let base = identity("source-a", "r1", ParserVersion::Version(1));
    let other_workspace = identity("source-b", "r1", ParserVersion::Version(1));
    let other_revision = identity("source-a", "r2", ParserVersion::Version(1));
    let other_parser = identity("source-a", "r1", ParserVersion::Version(2));
    let legacy = identity("source-a", "r1", ParserVersion::Legacy);

    assert_eq!(
        base.mismatch(&base),
        IdentityDiff {
            workspace: false,
            revision: false,
            parser_version: false
        }
    );
    assert!(!base.mismatch(&base).is_mismatch());

    let diff = base.mismatch(&other_workspace);
    assert!(diff.workspace && !diff.revision && !diff.parser_version);
    assert!(diff.is_mismatch());

    let diff = base.mismatch(&other_revision);
    assert!(!diff.workspace && diff.revision && !diff.parser_version);
    assert!(diff.is_mismatch());

    let diff = base.mismatch(&other_parser);
    assert!(!diff.workspace && !diff.revision && diff.parser_version);
    assert!(diff.is_mismatch());

    // Legacy 与任何 Version 都视为不一致（未知版本）。
    assert!(base.mismatch(&legacy).is_mismatch());
}

#[test]
fn display_is_deterministic() {
    let id = identity("source-a", "r1", ParserVersion::Version(1));
    let text = id.to_string();
    assert!(text.contains("workspace=source-a"));
    assert!(text.contains("revision=r1"));
    assert!(text.contains("parser=version-1"));
}
