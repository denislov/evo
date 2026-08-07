//! `lsp::edit` 测试：WorkspaceEdit 校验（路径 / 版本 / range / 打开状态）、
//! mutation 计划、受限 applicator 的 ChangeReceipt 语义、params 组装。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use crate::lsp::documents::{ContentChange, DocumentStore, Position, Range};
use crate::lsp::edit::{
    EditApplicator, EditError, EditPlan, PlannedChange, TextEdit, WorkspaceEdit, did_change_params,
    did_close_params, did_open_params, parse_apply_edit_params, relative_path,
    restricted::revision_of, validate_edit,
};

fn store_with(root: &std::path::Path, name: &str, version: i64, text: &str) -> DocumentStore {
    let mut documents = DocumentStore::new(root.to_path_buf());
    let uri = format!("file://{}/{}", root.display(), name);
    documents.open(&uri, "rust", version, text).unwrap();
    documents
}

fn ws_edit(root: &std::path::Path, name: &str, edit: TextEdit) -> WorkspaceEdit {
    let mut changes = BTreeMap::new();
    changes.insert(format!("file://{}/{}", root.display(), name), vec![edit]);
    WorkspaceEdit {
        changes,
        document_changes: vec![],
    }
}

/// 受限 applicator：把计划应用到临时目录，生成 ChangeReceipt。
struct TestApplicator {
    root: PathBuf,
}

impl EditApplicator for TestApplicator {
    fn apply(&self, plan: &EditPlan) -> Result<Vec<change_tracker::ChangeReceipt>, EditError> {
        let mut receipts = Vec::new();
        for change in &plan.changes {
            let abs = self.root.join(relative_target(&change.uri, &self.root));
            if !abs.starts_with(&self.root) {
                return Err(EditError::Apply {
                    detail: format!("applicator refuses path {}", abs.display()),
                });
            }
            let before = std::fs::read_to_string(&abs).map_err(|error| EditError::Apply {
                detail: format!("read {}: {error}", abs.display()),
            })?;
            let after = match change.range {
                None => change.new_text.clone(),
                Some(range) => {
                    let text = before.clone();
                    let start = position_to_offset(&text, range.start);
                    let end = position_to_offset(&text, range.end);
                    let mut out = String::new();
                    out.push_str(&text[..start]);
                    out.push_str(&change.new_text);
                    out.push_str(&text[end..]);
                    out
                }
            };
            std::fs::write(&abs, &after).map_err(|error| EditError::Apply {
                detail: format!("write {}: {error}", abs.display()),
            })?;
            let before_bytes = before.as_bytes();
            let after_bytes = after.as_bytes();
            receipts.push(change_tracker::ChangeReceipt {
                path: change.rel_path.clone(),
                target_fingerprint: format!("test-{}", change.uri),
                before_revision: Some(revision_of(before_bytes)),
                after_revision: revision_of(after_bytes),
                after_exists: true,
                byte_delta: after_bytes.len() as i64 - before_bytes.len() as i64,
                line_delta: after.lines().count() as i64 - before.lines().count() as i64,
                origin: "lsp/applyEdit".into(),
                unified_diff: None,
            });
        }
        Ok(receipts)
    }
}

fn relative_target(uri: &str, root: &PathBuf) -> PathBuf {
    let rel = uri.strip_prefix("file://").unwrap();
    let path = PathBuf::from(rel);
    path.strip_prefix(root).unwrap_or(&path).to_path_buf()
}

fn position_to_offset(text: &str, position: Position) -> usize {
    let mut line = 0u32;
    let mut offset = 0usize;
    for (index, character) in text.char_indices() {
        if line == position.line {
            let line_text = &text[offset..];
            return offset
                + crate::lsp::documents::utf16_to_char_index(
                    line_text,
                    position.character as usize,
                );
        }
        if character == '\n' {
            line += 1;
            offset = index + 1;
        }
    }
    offset
}

#[test]
fn valid_changes_edit_produces_plan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "fn old() {}\n");
    let edit = ws_edit(
        &root,
        "a.rs",
        TextEdit {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 3,
                },
                end: Position {
                    line: 0,
                    character: 6,
                },
            }),
            new_text: "new".into(),
        },
    );
    let plan = validate_edit(&edit, &documents).unwrap();
    assert_eq!(plan.changes.len(), 1);
    let change = &plan.changes[0];
    assert_eq!(change.rel_path, "a.rs");
    assert_eq!(change.new_text, "new");
    assert!(change.uri.starts_with("file://"));
}

#[test]
fn document_changes_version_must_match() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 3, "text");
    let mut edit = WorkspaceEdit::default();
    edit.document_changes
        .push(crate::lsp::edit::TextDocumentEdit {
            uri: format!("file://{}/a.rs", root.display()),
            version: Some(3),
            edits: vec![TextEdit {
                range: None,
                new_text: "v3 text".into(),
            }],
        });
    assert!(validate_edit(&edit, &documents).is_ok());

    edit.document_changes[0].version = Some(2);
    match validate_edit(&edit, &documents) {
        Err(EditError::VersionMismatch { given, current, .. }) => {
            assert_eq!(given, 2);
            assert_eq!(current, 3);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn path_escape_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "text");
    let cases = [
        format!("file://{}/../outside.rs", root.display()),
        "file:///etc/passwd".to_string(),
        "https://example.com/a.rs".to_string(),
        "file://relative.rs".to_string(),
    ];
    for uri in cases {
        let mut changes = BTreeMap::new();
        changes.insert(
            uri.clone(),
            vec![TextEdit {
                range: None,
                new_text: "x".into(),
            }],
        );
        let edit = WorkspaceEdit {
            changes,
            document_changes: vec![],
        };
        match validate_edit(&edit, &documents) {
            Err(EditError::OutsideWorkspace { .. }) => {}
            Err(EditError::Invalid { .. }) => {}
            other => panic!("expected rejection for {uri}, got {other:?}"),
        }
    }
}

#[test]
fn document_not_open_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "text");
    let edit = ws_edit(
        &root,
        "missing.rs",
        TextEdit {
            range: None,
            new_text: "x".into(),
        },
    );
    assert!(matches!(
        validate_edit(&edit, &documents),
        Err(EditError::DocumentNotOpen { .. })
    ));
}

#[test]
fn out_of_bounds_range_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "ab\n");
    let edit = ws_edit(
        &root,
        "a.rs",
        TextEdit {
            range: Some(Range {
                start: Position {
                    line: 9,
                    character: 0,
                },
                end: Position {
                    line: 9,
                    character: 1,
                },
            }),
            new_text: "x".into(),
        },
    );
    assert!(matches!(
        validate_edit(&edit, &documents),
        Err(EditError::RangeOutOfBounds { .. })
    ));
}

#[test]
fn reverse_ordered_edits_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "abcdef");
    let mut changes = BTreeMap::new();
    changes.insert(
        format!("file://{}/a.rs", root.display()),
        vec![
            TextEdit {
                range: Some(Range {
                    start: Position {
                        line: 0,
                        character: 4,
                    },
                    end: Position {
                        line: 0,
                        character: 5,
                    },
                }),
                new_text: "E".into(),
            },
            TextEdit {
                range: Some(Range {
                    start: Position {
                        line: 0,
                        character: 1,
                    },
                    end: Position {
                        line: 0,
                        character: 2,
                    },
                }),
                new_text: "B".into(),
            },
        ],
    );
    let edit = WorkspaceEdit {
        changes,
        document_changes: vec![],
    };
    assert!(matches!(
        validate_edit(&edit, &documents),
        Err(EditError::RangeOutOfBounds { .. })
    ));
}

#[test]
fn parse_apply_edit_params_changes_and_document_changes() {
    let params = json!({
        "changes": {
            "file:///tmp/a.rs": [
                {"range": {"start": {"line": 0, "character": 0},
                           "end": {"line": 0, "character": 1}},
                 "newText": "X"}
            ]
        },
        "documentChanges": [
            {"textDocument": {"uri": "file:///tmp/b.rs", "version": 4},
             "edits": [{"newText": "full"}]}
        ]
    });
    let edit = parse_apply_edit_params(&params).unwrap();
    assert_eq!(edit.changes.len(), 1);
    assert_eq!(edit.document_changes.len(), 1);
    assert_eq!(edit.document_changes[0].version, Some(4));
    assert_eq!(edit.document_changes[0].edits[0].range, None);

    // 空 edit 拒绝。
    assert!(parse_apply_edit_params(&json!({})).is_err());
    // 坏 edits 拒绝。
    assert!(
        parse_apply_edit_params(&json!({"changes": {"file:///tmp/a.rs": [{"range": null}]}}))
            .is_err()
    );
}

#[test]
fn restricted_applicator_emits_change_receipt_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let documents = store_with(&root, "a.rs", 1, "hello\n");
    let edit = ws_edit(
        &root,
        "a.rs",
        TextEdit {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 5,
                },
                end: Position {
                    line: 0,
                    character: 5,
                },
            }),
            new_text: " world\n".into(),
        },
    );
    let plan = validate_edit(&edit, &documents).unwrap();
    let path = root.join("a.rs");
    std::fs::write(&path, "hello\n").unwrap();
    let applicator = TestApplicator { root: root.clone() };
    let receipts = applicator.apply(&plan).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.path, "a.rs");
    assert_eq!(
        receipt.before_revision.as_deref(),
        Some(revision_of(b"hello\n").as_str())
    );
    // 插入点 (0,5) 是 'o' 与 '\n' 之间：结果 "hello world\n\n"。
    assert_eq!(receipt.after_revision, revision_of(b"hello world\n\n"));
    assert_eq!(receipt.byte_delta, 7); // 13 - 6
    assert_eq!(receipt.line_delta, 1);
    assert_eq!(receipt.origin, "lsp/applyEdit");
    // 磁盘内容真实被改（受限应用发生在 workspace 内）。
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world\n\n");
}

#[test]
fn applicator_refuses_outside_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let applicator = TestApplicator { root: root.clone() };
    let plan = EditPlan {
        changes: vec![PlannedChange {
            uri: "file:///etc/passwd".into(),
            rel_path: "../passwd".into(),
            range: None,
            new_text: "owned".into(),
        }],
    };
    assert!(matches!(
        applicator.apply(&plan),
        Err(EditError::Apply { .. })
    ));
}

#[test]
fn relative_path_uses_forward_slashes() {
    let root = PathBuf::from("/ws");
    assert_eq!(
        relative_path(&PathBuf::from("/ws/src/a.rs"), &root),
        "src/a.rs"
    );
    assert_eq!(relative_path(&PathBuf::from("/ws"), &root), "");
}

#[test]
fn did_notification_params_assembly() {
    let open = did_open_params("file:///tmp/a.rs", "rust", 1, "fn main() {}");
    assert_eq!(open["textDocument"]["languageId"], "rust");
    assert_eq!(open["textDocument"]["version"], 1);
    let change = did_change_params("file:///tmp/a.rs", 2, "new text");
    assert_eq!(change["textDocument"]["version"], 2);
    assert_eq!(change["contentChanges"][0]["text"], "new text");
    assert!(change["contentChanges"][0].get("range").is_none());
    let close = did_close_params("file:///tmp/a.rs");
    assert_eq!(close["textDocument"]["uri"], "file:///tmp/a.rs");
}

#[test]
fn change_events_round_trip_through_document_store() {
    // edit 层的 ContentChange 与 documents 层一致（同一类型）。
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mut documents = store_with(&root, "a.rs", 1, "abc");
    documents
        .change(
            &format!("file://{}/a.rs", root.display()),
            2,
            &[ContentChange {
                range: Some(Range {
                    start: Position {
                        line: 0,
                        character: 1,
                    },
                    end: Position {
                        line: 0,
                        character: 2,
                    },
                }),
                text: "XY".into(),
            }],
        )
        .unwrap();
    assert_eq!(
        documents
            .get(&format!("file://{}/a.rs", root.display()))
            .unwrap()
            .text,
        "aXYc"
    );
}

#[test]
fn multi_file_edit_plans_all_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mut documents = DocumentStore::new(root.to_path_buf());
    let a = format!("file://{}/a.rs", root.display());
    let b = format!("file://{}/b.rs", root.display());
    documents.open(&a, "rust", 1, "aaa").unwrap();
    documents.open(&b, "rust", 1, "bbb").unwrap();
    let mut changes = BTreeMap::new();
    changes.insert(
        a,
        vec![TextEdit {
            range: None,
            new_text: "AAA".into(),
        }],
    );
    changes.insert(
        b,
        vec![TextEdit {
            range: None,
            new_text: "BBB".into(),
        }],
    );
    let plan = validate_edit(
        &WorkspaceEdit {
            changes,
            document_changes: vec![],
        },
        &documents,
    )
    .unwrap();
    assert_eq!(plan.changes.len(), 2);
    let paths: Vec<_> = plan
        .changes
        .iter()
        .map(|change| change.rel_path.as_str())
        .collect();
    assert_eq!(paths, ["a.rs", "b.rs"]);
}

#[test]
fn empty_plan_is_detectable() {
    assert!(EditPlan::default().is_empty());
}
