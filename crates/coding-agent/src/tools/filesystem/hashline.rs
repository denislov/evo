//! Hashline anchors for edits that survive bounded line shifts.
//!
//! Anchors are intentionally content-addressed but local: a line number is a
//! hint, the line hash is the identity, and resolution is limited to a window
//! so a stale proposal cannot silently edit an unrelated part of a file.

use crate::mutex::MutexExt;
use crate::services::review::MutationTracking;
use crate::tools::filesystem::bounded::read_target_bytes;
use crate::tools::filesystem::mutation_receipt::{bounded_diff, receipt, validate_fence};
use crate::tools::filesystem_target_for_runtime_execution;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind, ToolRequirement,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};
use workspace_runtime::api::FileMutation;
use workspace_runtime::api::WorkspaceAccessHandle;

pub(crate) const DEFAULT_SHIFT_WINDOW: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HashlineAnchor {
    pub(crate) line: usize,
    pub(crate) hash: String,
    pub(crate) context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HashlineRecord {
    pub(crate) line: usize,
    pub(crate) hash: String,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShiftResult {
    Found { line: usize, anchor: HashlineAnchor },
    Ambiguous { candidates: Vec<usize> },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HashlineEdit {
    pub(crate) anchor: HashlineAnchor,
    pub(crate) replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HashlineEditError {
    pub(crate) message: String,
    pub(crate) shifted_to: Option<HashlineAnchor>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HashlineEditArgs {
    path: String,
    edits: Vec<HashlineEditInput>,
    #[serde(default, rename = "expectedRevision")]
    #[schemars(rename = "expectedRevision")]
    expected_revision: Option<String>,
    #[serde(default, rename = "expectedTargetFingerprint")]
    #[schemars(rename = "expectedTargetFingerprint")]
    expected_target_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HashlineEditInput {
    line: usize,
    hash: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(rename = "newText")]
    #[schemars(rename = "newText")]
    new_text: String,
}

impl std::fmt::Display for HashlineEditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HashlineEditError {}

pub(crate) fn line_hash(text: &str) -> String {
    // Eight bytes are enough for a local anchor while keeping model-visible
    // lines compact. The full file revision remains the mutation fence.
    let digest = Sha256::digest(text.as_bytes());
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn snapshot(content: &str) -> Vec<HashlineRecord> {
    split_lines(content)
        .into_iter()
        .enumerate()
        .map(|(index, text)| HashlineRecord {
            line: index + 1,
            hash: line_hash(&text),
            text,
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn format_anchor(record: &HashlineRecord) -> String {
    format!("{}:{}→{}", record.line, record.hash, record.text)
}

pub(crate) fn resolve(
    records: &[HashlineRecord],
    anchor: &HashlineAnchor,
    window: usize,
) -> ShiftResult {
    if anchor.line == 0 {
        return ShiftResult::NotFound;
    }
    let lower = anchor.line.saturating_sub(window).max(1);
    let upper = anchor.line.saturating_add(window).min(records.len());
    let mut candidates = records
        .iter()
        .filter(|record| record.line >= lower && record.line <= upper)
        .filter(|record| record.hash == anchor.hash)
        .filter(|record| {
            anchor
                .context
                .as_deref()
                .is_none_or(|context| record.text.contains(context))
        })
        .map(|record| record.line)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    match candidates.as_slice() {
        [line] => {
            let record = &records[*line - 1];
            ShiftResult::Found {
                line: *line,
                anchor: HashlineAnchor {
                    line: *line,
                    hash: record.hash.clone(),
                    context: anchor.context.clone(),
                },
            }
        }
        [] => ShiftResult::NotFound,
        _ => ShiftResult::Ambiguous { candidates },
    }
}

pub(crate) fn apply_batch(
    content: &str,
    edits: &[HashlineEdit],
) -> Result<(String, Vec<HashlineAnchor>), HashlineEditError> {
    let records = snapshot(content);
    let mut resolved = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        reject_embedded_anchor(&edit.replacement)?;
        match resolve(&records, &edit.anchor, DEFAULT_SHIFT_WINDOW) {
            ShiftResult::Found { line, anchor } => resolved.push((line, index, anchor)),
            ShiftResult::Ambiguous { candidates } => {
                return Err(HashlineEditError {
                    message: format!(
                        "hashline anchor {}:{} is ambiguous; candidates: {candidates:?}",
                        edit.anchor.line, edit.anchor.hash
                    ),
                    shifted_to: None,
                });
            }
            ShiftResult::NotFound => {
                return Err(HashlineEditError {
                    message: format!(
                        "hashline anchor {}:{} was not found within ±{} lines",
                        edit.anchor.line, edit.anchor.hash, DEFAULT_SHIFT_WINDOW
                    ),
                    shifted_to: None,
                });
            }
        }
    }
    resolved.sort_by_key(|(line, _, _)| *line);
    for pair in resolved.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(HashlineEditError {
                message: format!("hashline edits overlap at line {}", pair[0].0),
                shifted_to: None,
            });
        }
    }
    let mut lines = split_lines(content);
    for (line, index, _) in &resolved {
        lines[*line - 1] = edits[*index].replacement.clone();
    }
    let trailing_newline = content.ends_with('\n');
    let mut output = lines.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    Ok((
        output,
        resolved.into_iter().map(|(_, _, anchor)| anchor).collect(),
    ))
}

#[cfg(test)]
pub(crate) fn hashline_edit_runtime_tool(
    filesystem: WorkspaceAccessHandle,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    hashline_edit_runtime_tool_with_tracking(filesystem, None)
}

pub(crate) fn hashline_edit_runtime_tool_with_tracking(
    filesystem: WorkspaceAccessHandle,
    tracking: Option<MutationTracking>,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("hashline_edit").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: "Edit lines using bounded content hash anchors. Anchors are validated against one pre-edit snapshot before any write.".into(),
        parameters: schema_for::<HashlineEditArgs>().expect("HashlineEditArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: vec![ToolRequirement {
            tool: ToolId::new("read").expect("static tool id is valid"),
            minimum_behavior: ToolBehaviorVersion::V1,
        }],
    };
    Ok(Arc::new(TypedTool::<HashlineEditArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            let tracking = tracking.clone();
            Box::pin(async move {
                if args.edits.is_empty() || args.edits.len() > crate::limits::MAX_HASHLINE_EDITS {
                    return Err(ToolError::new(
                        ToolErrorKind::InvalidArguments,
                        format!(
                            "hashline_edit: edits must contain between 1 and {} entries",
                            crate::limits::MAX_HASHLINE_EDITS
                        ),
                    ));
                }
                let target = filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "hashline_edit",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                let mutation = FileMutation::begin(&target)
                    .await
                    .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                let raw = read_target_bytes(
                    &target,
                    "hashline_edit",
                    crate::limits::MAX_TOOL_EDIT_FILE_BYTES,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                validate_fence(
                    args.expected_revision.as_deref(),
                    args.expected_target_fingerprint.as_deref(),
                    Some(&crate::tools::filesystem::mutation_receipt::content_revision(&raw)),
                    target.target_fingerprint(),
                    &args.path,
                )
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                let content = String::from_utf8(raw.clone()).map_err(|error| {
                    ToolError::new(
                        ToolErrorKind::Execution,
                        format!(
                            "hashline_edit: file is not valid UTF-8 (offset {})",
                            error.utf8_error().valid_up_to()
                        ),
                    )
                })?;
                let edits = args
                    .edits
                    .iter()
                    .map(|edit| HashlineEdit {
                        anchor: HashlineAnchor {
                            line: edit.line,
                            hash: edit.hash.clone(),
                            context: edit.context.clone(),
                        },
                        replacement: edit.new_text.clone(),
                    })
                    .collect::<Vec<_>>();
                let (updated, shifted) = apply_batch(&content, &edits).map_err(|error| {
                    ToolError::new(ToolErrorKind::InvalidArguments, error.to_string())
                })?;
                if updated.len() > crate::limits::MAX_EDIT_RESULT_BYTES {
                    return Err(ToolError::new(
                        ToolErrorKind::InvalidArguments,
                        format!(
                            "hashline_edit: result exceeds the {} safety limit",
                            crate::platform::io::output::format_size(
                                crate::limits::MAX_EDIT_RESULT_BYTES
                            )
                        ),
                    ));
                }
                let target_clone = target.clone();
                let updated_bytes = updated.as_bytes().to_vec();
                tokio::task::spawn_blocking(move || {
                    let _mutation = mutation;
                    let file = target_clone.opened_file()?;
                    let mut file = file
                        .lock_resource("hashline edit write")
                        .map_err(|error| error.to_string())?;
                    file.seek(SeekFrom::Start(0))
                        .map_err(|error| error.to_string())?;
                    file.set_len(0).map_err(|error| error.to_string())?;
                    file.write_all(&updated_bytes)
                        .map_err(|error| error.to_string())?;
                    file.sync_all().map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error.to_string()))?
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                let patch = crate::tools::filesystem::diff::generate_unified_patch(
                    &args.path, &content, &updated,
                );
                let change_receipt = receipt(
                    args.path.clone(),
                    target.target_fingerprint().to_owned(),
                    Some(&raw),
                    Some(updated.as_bytes()),
                    "hashline_edit",
                    bounded_diff(patch),
                );
                if let Some(tracking) = tracking.as_ref() {
                    tracking
                        .record(&context.call_id, change_receipt.clone())
                        .await
                        .map_err(|error| {
                            ToolError::new(
                                ToolErrorKind::Execution,
                                format!(
                                    "hashline_edit: mutation committed but change tracking failed; reconcile required: {error}"
                                ),
                            )
                        })?;
                }
                Ok(ToolOutput {
                    content: vec![ToolContent::Text {
                        text: format!(
                            "Successfully replaced {} anchored line(s) in {}.",
                            edits.len(),
                            args.path
                        ),
                    }],
                    details: Some(serde_json::json!({
                        "changeReceipt": change_receipt,
                        "shiftedAnchors": shifted,
                    })),
                    terminate: false,
                })
            }) as ToolFuture
        },
    )?))
}

fn reject_embedded_anchor(replacement: &str) -> Result<(), HashlineEditError> {
    if replacement
        .lines()
        .any(|line| parse_anchor_prefix(line).is_some())
    {
        return Err(HashlineEditError {
            message: "replacement contains a hashline anchor prefix; provide source text without read-output markers".into(),
            shifted_to: None,
        });
    }
    Ok(())
}

fn parse_anchor_prefix(line: &str) -> Option<(&str, &str)> {
    let (line_number, rest) = line.split_once(':')?;
    line_number.parse::<usize>().ok()?;
    let (hash, text) = rest.split_once('→')?;
    (hash.len() == 12 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some((hash, text))
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::ToolId;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};
    use workspace_runtime::api::WorkspaceAccessHandle;

    fn anchor(content: &str, line: usize) -> HashlineAnchor {
        let record = snapshot(content).remove(line - 1);
        HashlineAnchor {
            line,
            hash: record.hash,
            context: None,
        }
    }

    #[test]
    fn formats_compact_line_anchors() {
        let records = snapshot("alpha\nbeta\n");
        assert_eq!(
            format_anchor(&records[0]),
            format!("1:{}→alpha", records[0].hash)
        );
        assert_eq!(records[0].hash.len(), 12);
    }

    #[test]
    fn resolves_a_shifted_anchor_within_the_bounded_window() {
        let before = "one\ntwo\nthree\n";
        let anchor = anchor(before, 2);
        let after = snapshot("zero\none\ntwo\nthree\n");
        assert!(matches!(
            resolve(&after, &anchor, 15),
            ShiftResult::Found { line: 3, .. }
        ));
    }

    #[test]
    fn batch_validates_before_applying_and_rejects_overlap() {
        let content = "one\ntwo\nthree\n";
        let duplicate = anchor(content, 2);
        let error = apply_batch(
            content,
            &[
                HashlineEdit {
                    anchor: duplicate.clone(),
                    replacement: "TWO".into(),
                },
                HashlineEdit {
                    anchor: duplicate,
                    replacement: "2".into(),
                },
            ],
        )
        .unwrap_err();
        assert!(error.message.contains("overlap"));
    }

    #[test]
    fn rejects_pasted_anchor_prefixes() {
        let content = "one\ntwo\n";
        let error = apply_batch(
            content,
            &[HashlineEdit {
                anchor: anchor(content, 1),
                replacement: "1:deadbeefdead→one".into(),
            }],
        )
        .unwrap_err();
        assert!(error.message.contains("anchor prefix"));
    }

    #[tokio::test]
    async fn typed_hashline_edit_writes_from_one_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "zero\none\ntwo\nthree\n").unwrap();
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let records = snapshot("one\ntwo\nthree\n");
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(hashline_edit_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let output = runtime
            .execute(
                ToolCallContext::new(
                    ToolId::new("hashline_edit").unwrap(),
                    "hashline-call",
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "path": "target.txt",
                    "edits": [{"line": 2, "hash": records[1].hash, "newText": "TWO"}]
                }),
            )
            .await
            .expect("hashline edit succeeds");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("target.txt")).unwrap(),
            "zero\none\nTWO\nthree\n"
        );
        assert_eq!(
            output.details.unwrap()["changeReceipt"]["origin"],
            "hashline_edit"
        );
    }

    #[tokio::test]
    async fn typed_hashline_edit_rejects_oversized_source() {
        let temp = tempfile::tempdir().unwrap();
        let oversized = vec![b'x'; crate::limits::MAX_TOOL_EDIT_FILE_BYTES + 1];
        std::fs::write(temp.path().join("large.txt"), &oversized).unwrap();
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(hashline_edit_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let error = runtime
            .execute(
                ToolCallContext::new(
                    ToolId::new("hashline_edit").unwrap(),
                    "large-hashline-call",
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "path": "large.txt",
                    "edits": [{"line": 1, "hash": "000000000000", "newText": "y"}]
                }),
            )
            .await
            .expect_err("oversized hashline source must fail");
        assert!(error.message.contains("safety limit"), "{error:?}");
        assert_eq!(
            std::fs::read(temp.path().join("large.txt")).unwrap(),
            oversized
        );
    }
}
