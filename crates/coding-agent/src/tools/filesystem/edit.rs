use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::fs::edit_file::OpenedEditFile as PlatformOpenedEditFile;
use crate::platform::fs::mutation::{FileMutation, MutationGuard};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::diff::{
    TextReplacement, apply_replacements_preserving_unchanged_lines, generate_diff_string,
    generate_unified_patch,
};
use crate::tools::filesystem::mutation_receipt::{
    bounded_diff, content_revision, receipt, validate_fence,
};
use crate::tools::filesystem::text_match::normalize_unicode_confusables;
use agent_core::api::tool::AgentToolOutput;
use futures::future::{BoxFuture, FutureExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind, ToolRequirement,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};

const DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. Merge nearby changes into one edit; do not include large unchanged regions.";

struct Edit {
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<EditReplacementArgs>,
    #[serde(default, rename = "expectedRevision")]
    #[schemars(rename = "expectedRevision")]
    expected_revision: Option<String>,
    #[serde(default, rename = "expectedTargetFingerprint")]
    #[schemars(rename = "expectedTargetFingerprint")]
    expected_target_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditReplacementArgs {
    #[serde(rename = "oldText")]
    #[schemars(rename = "oldText")]
    old_text: String,
    #[serde(rename = "newText")]
    #[schemars(rename = "newText")]
    new_text: String,
}

fn detect_crlf(s: &str) -> bool {
    match (s.find("\r\n"), s.find('\n')) {
        (Some(rn), Some(n)) => rn <= n,
        _ => s.contains("\r\n"),
    }
}

fn normalize_to_lf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_crlf(s: &str, crlf: bool) -> String {
    if crlf {
        s.replace('\n', "\r\n")
    } else {
        s.to_string()
    }
}

fn strip_bom(s: &str) -> (&str, &str) {
    if let Some(r) = s.strip_prefix('\u{feff}') {
        ("\u{feff}", r)
    } else {
        ("", s)
    }
}

fn count_occurrences(content: &str, old: &str) -> usize {
    if old.is_empty() {
        return 0;
    }
    content.matches(old).count()
}

/// Try an exact match first; on failure, try a fuzzy-normalized match.
/// Returns `(found, match_index, match_length, used_fuzzy)`. Mirrors TS
/// `fuzzyFindText` (offsets are in the content used for replacement).
fn fuzzy_find_text(content: &str, old_text: &str) -> (bool, usize, usize, bool) {
    if let Some(idx) = content.find(old_text) {
        return (true, idx, old_text.len(), false);
    }
    let fuzzy_content = normalize_unicode_confusables(content);
    let fuzzy_old = normalize_unicode_confusables(old_text);
    if let Some(idx) = fuzzy_content.find(&fuzzy_old) {
        return (true, idx, fuzzy_old.len(), true);
    }
    (false, 0, 0, false)
}

fn apply_edits(normalized: &str, edits: &[Edit], path: &str) -> Result<(String, String), String> {
    let total = edits.len();
    let norm: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();

    for (i, e) in norm.iter().enumerate() {
        if e.old_text.is_empty() {
            return Err(if total == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{i}].oldText must not be empty in {path}.")
            });
        }
    }

    // Match each edit: exact first, then fuzzy-normalized. If any edit uses
    // fuzzy matching, replacements run in fuzzy-normalized space and are then
    // overlaid onto the original content so unchanged lines keep their original
    // bytes (TS `applyEditsToNormalizedContent` +
    // `applyReplacementsPreservingUnchangedLines`).
    let used_fuzzy = norm.iter().any(|e| !normalized.contains(&e.old_text));
    let base: String = if used_fuzzy {
        normalize_unicode_confusables(normalized)
    } else {
        normalized.to_string()
    };

    let mut matched: Vec<(usize, usize, usize, String)> = Vec::new();
    for (i, e) in norm.iter().enumerate() {
        let (found, idx, len, _fuzzy) = fuzzy_find_text(&base, &e.old_text);
        if !found {
            return Err(not_found(path, i, total));
        }
        let search_text = if used_fuzzy {
            normalize_unicode_confusables(&e.old_text)
        } else {
            e.old_text.clone()
        };
        let occ = count_occurrences(&base, &search_text);
        if occ > 1 {
            return Err(duplicate(path, i, total, occ));
        }
        matched.push((i, idx, len, e.new_text.clone()));
    }

    matched.sort_by_key(|m| m.1);
    for w in matched.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.1.saturating_add(a.2) > b.1 {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                a.0, b.0
            ));
        }
    }

    let base_content = normalized.to_string();
    let new_content = if used_fuzzy {
        let replacements: Vec<TextReplacement<'_>> = matched
            .iter()
            .map(|(_, idx, len, new)| TextReplacement {
                match_index: *idx,
                match_length: *len,
                new_text: new.as_str(),
            })
            .collect();
        apply_replacements_preserving_unchanged_lines(normalized, &base, &replacements)
            .ok_or_else(|| {
                format!(
                    "Could not align fuzzy match to original lines in {path}. Provide oldText exactly as it appears in the file."
                )
            })?
    } else {
        let mut out = base.clone();
        for (_, idx, len, new) in matched.iter().rev() {
            out.replace_range(*idx..*idx + *len, new);
        }
        out
    };

    if base_content == new_content {
        return Err(if total == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        });
    }
    Ok((base_content, new_content))
}

fn not_found(path: &str, i: usize, total: usize) -> String {
    if total == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn duplicate(path: &str, i: usize, total: usize, n: usize) -> String {
    if total == 1 {
        format!(
            "Found {n} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {n} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn parse_edits(args: &serde_json::Value) -> Result<Vec<Edit>, String> {
    let mut edits_val = args
        .get("edits")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(s) = edits_val.as_str()
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(s)
    {
        edits_val = v;
    }
    let mut out: Vec<Edit> = Vec::new();
    if let Some(arr) = edits_val.as_array() {
        for e in arr {
            let o = e.get("oldText").and_then(|v| v.as_str());
            let n = e.get("newText").and_then(|v| v.as_str());
            if let (Some(o), Some(n)) = (o, n) {
                out.push(Edit {
                    old_text: o.into(),
                    new_text: n.into(),
                });
            }
        }
    }
    if let (Some(o), Some(n)) = (
        args.get("oldText").and_then(|v| v.as_str()),
        args.get("newText").and_then(|v| v.as_str()),
    ) {
        out.push(Edit {
            old_text: o.into(),
            new_text: n.into(),
        });
    }
    if out.is_empty() {
        return Err(
            "Edit tool input is invalid. edits must contain at least one replacement.".into(),
        );
    }
    Ok(out)
}

pub trait EditOperations: Send + Sync {
    fn open_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Box<dyn OpenedEditFile>, String>>;
}

pub trait OpenedEditFile: Send + Sync {
    fn read_file<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, String>>;
    fn write_file<'a>(
        &'a self,
        content: &'a [u8],
        mutation: MutationGuard,
    ) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Debug, Default)]
pub struct RealEditOperations;

impl EditOperations for RealEditOperations {
    fn open_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Box<dyn OpenedEditFile>, String>> {
        let target = target.clone();
        async move {
            let file = target.opened_file()?;
            Ok(Box::new(RealOpenedEditFile {
                inner: PlatformOpenedEditFile::new(file, target.display_path().to_path_buf()),
            }) as Box<dyn OpenedEditFile>)
        }
        .boxed()
    }
}

struct RealOpenedEditFile {
    inner: PlatformOpenedEditFile,
}

impl OpenedEditFile for RealOpenedEditFile {
    fn read_file<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        async move { self.inner.read_file().await }.boxed()
    }

    fn write_file<'a>(
        &'a self,
        content: &'a [u8],
        mutation: MutationGuard,
    ) -> BoxFuture<'a, Result<(), String>> {
        async move { self.inner.write_file(content, mutation).await }.boxed()
    }
}

pub(crate) async fn edit_execute_with_target_contract(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn EditOperations>,
) -> Result<ToolOutput, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("edit: missing or non-string 'path' argument")?
        .to_string();
    let edits = parse_edits(&args)?;
    let expected_revision = args
        .get("expectedRevision")
        .and_then(|value| value.as_str());
    let expected_target_fingerprint = args
        .get("expectedTargetFingerprint")
        .and_then(|value| value.as_str());
    let mutation = FileMutation::begin(target).await?;
    let target_fingerprint = target.target_fingerprint().to_owned();
    let target = target.clone();
    let opened = ops.open_file(&target).await?;
    let raw = opened.read_file().await?;
    let actual_revision = content_revision(&raw);
    validate_fence(
        expected_revision,
        expected_target_fingerprint,
        Some(&actual_revision),
        &target_fingerprint,
        &path,
    )?;
    let content = String::from_utf8(raw.clone()).map_err(|error| {
        format!(
            "edit: cannot edit {path} because the file is not valid UTF-8 (invalid byte at offset {}); use bash with an encoding-aware tool instead",
            error.utf8_error().valid_up_to()
        )
    })?;
    let (bom, body) = strip_bom(&content);
    let crlf = detect_crlf(body);
    let normalized = normalize_to_lf(body);
    let (base, new_content) = apply_edits(&normalized, &edits, &path)?;
    let final_content = format!("{bom}{}", restore_crlf(&new_content, crlf));
    if final_content.len() > crate::limits::MAX_EDIT_RESULT_BYTES {
        return Err(format!(
            "edit: result exceeds the {} safety limit",
            crate::platform::io::output::format_size(crate::limits::MAX_EDIT_RESULT_BYTES)
        ));
    }
    opened
        .write_file(final_content.as_bytes(), mutation)
        .await?;
    let diff = generate_diff_string(&base, &new_content, 4);
    let patch = generate_unified_patch(&path, &base, &new_content);
    let bounded_patch = bounded_diff(patch);
    let bounded_diff_text = bounded_diff(diff.diff);
    let change_receipt = receipt(
        path.clone(),
        target_fingerprint,
        Some(&raw),
        final_content.as_bytes(),
        "edit",
        bounded_patch.clone(),
    );
    let mut details = serde_json::json!({
        "diff": bounded_diff_text,
        "patch": bounded_patch,
        "changeReceipt": change_receipt,
    });
    if let Some(first_changed_line) = diff.first_changed_line {
        details["firstChangedLine"] = serde_json::json!(first_changed_line);
    }
    Ok(ToolOutput {
        content: vec![ToolContent::Text {
            text: format!("Successfully replaced {} block(s) in {path}.", edits.len()),
        }],
        details: Some(details),
        terminate: false,
    })
}

pub(crate) async fn edit_execute_with_target(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn EditOperations>,
) -> Result<AgentToolOutput, String> {
    edit_execute_with_target_contract(target, args, ops)
        .await
        .map(AgentToolOutput::from)
}

pub fn edit_runtime_tool(
    filesystem: FilesystemCapability,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("edit").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<EditArgs>().expect("EditArgs schema is valid"),
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
    Ok(Arc::new(TypedTool::<EditArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            Box::pin(async move {
                let target = crate::tools::filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "edit",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                edit_execute_with_target_contract(
                    &target,
                    serde_json::to_value(&args).expect("typed edit args serialize"),
                    Arc::new(RealEditOperations),
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        Edit, RealEditOperations, apply_edits, edit_execute_with_target, edit_runtime_tool,
    };
    use crate::platform::fs::capability::FilesystemCapability;
    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::ToolId;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    #[test]
    fn fuzzy_uniqueness_counts_in_the_same_normalized_space_as_search() {
        let cases = [
            ("curly quotes", "“hello”   \n“hello”   \n", "“hello”\n"),
            (
                "non-breaking space",
                "a\u{a0}b  \na\u{a0}b  \n",
                "a\u{a0}b\n",
            ),
            ("NFKC", "Ａ  \nＡ  \n", "Ａ\n"),
            ("trailing whitespace", "same  \nsame  \n", "same\n"),
        ];
        for (label, content, old_text) in cases {
            let error = apply_edits(
                content,
                &[Edit {
                    old_text: old_text.into(),
                    new_text: "replacement\n".into(),
                }],
                "target.txt",
            )
            .expect_err(label);
            assert!(
                error.contains("Found 2 occurrences"),
                "{label} should reject both normalized candidates: {error}"
            );
        }
    }

    #[tokio::test]
    async fn non_utf8_files_are_rejected_without_changing_any_byte() {
        for (name, bytes) in [
            ("latin1.txt", b"caf\xe9\n".as_slice()),
            ("gbk.txt", [0xC4, 0xE3, 0xBA, 0xC3, b'\n'].as_slice()),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
            let target = filesystem
                .prepare_target_for_tool("edit", name)
                .await
                .unwrap();
            let error = edit_execute_with_target(
                &target,
                serde_json::json!({
                    "path": name,
                    "edits": [{"oldText": "text", "newText": "replacement"}],
                }),
                Arc::new(RealEditOperations),
            )
            .await
            .expect_err("non-UTF-8 edit must fail");
            assert!(error.contains("not valid UTF-8"));
            assert!(error.contains("encoding-aware tool"));
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[tokio::test]
    async fn edit_returns_change_receipt_and_rejects_a_stale_revision() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "before\n").unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let target = filesystem
            .prepare_target_for_tool("edit", "target.txt")
            .await
            .unwrap();

        let output = edit_execute_with_target(
            &target,
            serde_json::json!({
                "path": "target.txt",
                "edits": [{"oldText": "before", "newText": "after"}]
            }),
            Arc::new(RealEditOperations),
        )
        .await
        .expect("edit succeeds");
        let receipt = output
            .details
            .expect("edit details")
            .get("changeReceipt")
            .cloned()
            .expect("receipt field");
        assert_eq!(receipt["origin"], "edit");
        assert_eq!(receipt["before_revision"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["after_revision"].as_str().unwrap().len(), 64);

        let stale = edit_execute_with_target(
            &target,
            serde_json::json!({
                "path": "target.txt",
                "expectedRevision": "0000000000000000000000000000000000000000000000000000000000000000",
                "edits": [{"oldText": "after", "newText": "stale"}]
            }),
            Arc::new(RealEditOperations),
        )
        .await
        .expect_err("stale edit must be rejected");
        assert!(stale.contains("mutation fence rejected"));
    }

    #[test]
    fn typed_edit_requires_read_behavior() {
        let temp = tempfile::tempdir().unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(edit_runtime_tool(filesystem).unwrap())
            .unwrap();
        let error = match ToolRuntime::new(registry) {
            Ok(_) => panic!("read requirement must be enforced"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires missing tool read"));
    }

    #[tokio::test]
    async fn typed_edit_executes_through_contract_runtime() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "before\n").unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(edit_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let context = ToolCallContext::new(
            ToolId::new("edit").unwrap(),
            "edit-call",
            CancellationToken::new(),
        );
        let output = runtime
            .execute(
                context,
                serde_json::json!({
                    "path": "target.txt",
                    "edits": [{"oldText": "before", "newText": "after"}]
                }),
            )
            .await
            .expect("typed edit succeeds");
        assert!(
            output.details.unwrap()["changeReceipt"]["after_revision"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("target.txt")).unwrap(),
            "after\n"
        );
    }
}
