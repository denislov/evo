//! `lsp::diagnostics` 测试：push/pull 解析、stale 状态机转换表、
//! Mark/Discard 策略、文档关闭清理。

use serde_json::json;

use crate::lsp::diagnostics::{
    DiagnosticItem, DiagnosticSeverity, DiagnosticStaleness, DiagnosticStore, StalePolicy,
    parse_publish_params, parse_pull_result, pull_params, staleness_after_doc_change, staleness_of,
};
use crate::lsp::documents::{DocumentStore, Position, Range};

fn diag(message: &str) -> DiagnosticItem {
    DiagnosticItem {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 1,
            },
        },
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        source: None,
        code: None,
    }
}

fn uri_for(root: &std::path::Path, name: &str) -> crate::lsp::documents::DocumentUri {
    DocumentStore::new(root.to_path_buf())
        .parse_uri(&format!("file://{}/{}", root.display(), name))
        .unwrap()
}

#[test]
fn publish_with_matching_version_is_fresh() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = DiagnosticStore::new();
    let uri = uri_for(temp.path(), "a.rs");
    store.publish(uri.clone(), Some(3), vec![diag("x")], 3);
    assert!(matches!(
        store.query(&uri, StalePolicy::Mark).unwrap().staleness,
        DiagnosticStaleness::Fresh { doc_version: 3 }
    ));
}

#[test]
fn stale_state_machine_transition_table() {
    let cases: &[(&str, Option<i64>, i64, bool, bool)] = &[
        ("version matches", Some(3), 3, true, false),
        ("version behind", Some(2), 3, false, false),
        ("version ahead", Some(4), 3, false, false),
        ("no version", None, 3, false, true),
    ];
    for (label, published, doc_version, fresh, unknown) in cases {
        let actual = staleness_of(*published, *doc_version);
        assert_eq!(
            matches!(actual, DiagnosticStaleness::Fresh { .. }),
            *fresh,
            "{label}: fresh flag"
        );
        assert_eq!(
            matches!(actual, DiagnosticStaleness::Unknown),
            *unknown,
            "{label}: unknown flag"
        );
        assert_eq!(
            matches!(actual, DiagnosticStaleness::Stale { .. }),
            !fresh && !unknown,
            "{label}: stale flag"
        );
    }
}

#[test]
fn doc_change_moves_fresh_and_unknown_to_stale() {
    let fresh = DiagnosticStaleness::Fresh { doc_version: 3 };
    assert!(matches!(
        staleness_after_doc_change(&fresh, 4),
        DiagnosticStaleness::Stale { .. }
    ));
    // 文档没变：保持 Fresh。
    assert!(matches!(
        staleness_after_doc_change(&fresh, 3),
        DiagnosticStaleness::Fresh { doc_version: 3 }
    ));
    // Unknown → Stale。
    assert!(matches!(
        staleness_after_doc_change(&DiagnosticStaleness::Unknown, 4),
        DiagnosticStaleness::Stale { .. }
    ));
    // Stale 保持。
    let stale = DiagnosticStaleness::Stale {
        reason: "was".into(),
    };
    assert_eq!(
        staleness_after_doc_change(&stale, 5),
        DiagnosticStaleness::Stale {
            reason: "was".into()
        }
    );
}

#[test]
fn mark_policy_returns_all_with_staleness() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = DiagnosticStore::new();
    let uri = uri_for(temp.path(), "a.rs");
    // 推送（fresh）→ 文档变化（stale）。
    store.publish(uri.clone(), Some(3), vec![diag("old")], 3);
    store.document_changed(&uri, 5);
    let entry = store.query(&uri, StalePolicy::Mark).unwrap();
    assert!(matches!(entry.staleness, DiagnosticStaleness::Stale { .. }));
    assert_eq!(entry.items.len(), 1);
    // 再次推送（新版本，fresh）。
    store.publish(uri.clone(), Some(5), vec![diag("new")], 5);
    let entry = store.query(&uri, StalePolicy::Mark).unwrap();
    assert!(entry.staleness.is_fresh());
    assert_eq!(entry.items[0].message, "new");
}

#[test]
fn discard_policy_filters_non_fresh() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = DiagnosticStore::new();
    let uri = uri_for(temp.path(), "a.rs");
    store.publish(uri.clone(), Some(3), vec![diag("stale one")], 3);
    // 文档变到 4 后推新的（Fresh）。
    store.document_changed(&uri, 4);
    store.publish(uri.clone(), Some(4), vec![diag("fresh")], 4);
    let entry = store.query(&uri, StalePolicy::Discard).unwrap();
    assert!(entry.staleness.is_fresh());
    assert_eq!(entry.items[0].message, "fresh");
}

#[test]
fn document_close_removes_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = DiagnosticStore::new();
    let uri = uri_for(temp.path(), "a.rs");
    store.publish(uri.clone(), Some(1), vec![diag("x")], 1);
    assert_eq!(store.len(), 1);
    store.document_closed(&uri);
    assert!(store.is_empty());
    assert!(store.query(&uri, StalePolicy::Mark).is_none());
}

#[test]
fn parse_publish_params_extracts_uri_version_items() {
    let params = json!({
        "uri": "file:///tmp/a.rs",
        "version": 7,
        "diagnostics": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
             "severity": 1, "message": "boom", "source": "rustc", "code": "E0308"},
            {"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 2}},
             "severity": 2, "message": "careful", "code": 42}
        ]
    });
    let (uri, version, items) = parse_publish_params(&params).unwrap();
    assert_eq!(uri, "file:///tmp/a.rs");
    assert_eq!(version, Some(7));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].severity, DiagnosticSeverity::Error);
    assert_eq!(items[0].source.as_deref(), Some("rustc"));
    assert_eq!(items[0].code.as_deref(), Some("E0308"));
    assert_eq!(items[1].severity, DiagnosticSeverity::Warning);
    assert_eq!(items[1].code.as_deref(), Some("42"));
}

#[test]
fn parse_publish_params_rejects_malformed() {
    assert!(parse_publish_params(&json!({})).is_none());
    assert!(parse_publish_params(&json!({"uri": "x"})).is_none());
    assert!(parse_publish_params(&json!({"uri": "x", "diagnostics": "nope"})).is_none());
}

#[test]
fn pull_params_and_result_round_trip() {
    let params = pull_params("file:///tmp/a.rs", Some("rid-1"));
    assert_eq!(params["textDocument"]["uri"], "file:///tmp/a.rs");
    assert_eq!(params["resultId"], "rid-1");
    let result = json!({
        "items": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
             "severity": 1, "message": "x"}
        ],
        "resultId": "rid-2"
    });
    let (items, result_id) = parse_pull_result(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(result_id.as_deref(), Some("rid-2"));
}

#[test]
fn refresh_all_recomputes_staleness_against_documents() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mut documents = DocumentStore::new(root.to_path_buf());
    let uri_str = format!("file://{}/a.rs", root.display());
    documents.open(&uri_str, "rust", 5, "text").unwrap();
    let mut store = DiagnosticStore::new();
    store.publish(uri_for(&root, "a.rs"), Some(3), vec![diag("x")], 3);
    crate::lsp::diagnostics::refresh_all(&mut store, &documents);
    let uri = uri_for(&root, "a.rs");
    assert!(matches!(
        store.query(&uri, StalePolicy::Mark).unwrap().staleness,
        DiagnosticStaleness::Stale { .. }
    ));
}
