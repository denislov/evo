use crate::kernel::error::CodingSessionError;
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditContext, SelfHealingEditOptions, SelfHealingEditOutcome,
    SelfHealingEditReplacement, SelfHealingEditRunner,
};
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::fs::edit_file::OpenedEditFile as PlatformOpenedEditFile;
use crate::platform::fs::mutation::{FileMutation, MutationGuard};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::diff::{
    TextReplacement, apply_replacements_preserving_unchanged_lines, generate_diff_string,
    generate_unified_patch,
};
use crate::tools::filesystem_target_for_execution;
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

const DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. Merge nearby changes into one edit; do not include large unchanged regions.";

struct Edit {
    old_text: String,
    new_text: String,
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "properties":{
            "path":{"type":"string"},
            "edits":{"type":"array","items":{"type":"object",
                "properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},
                "required":["oldText","newText"],"additionalProperties":false}}
        },
        "required":["path","edits"],"additionalProperties":false
    })
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

fn normalize_for_fuzzy(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed: String = nfkc
        .split('\n')
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{2002}'..='\u{200A}' => ' ',
            other => other,
        })
        .collect()
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
    let fuzzy_content = normalize_for_fuzzy(content);
    let fuzzy_old = normalize_for_fuzzy(old_text);
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
        normalize_for_fuzzy(normalized)
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
            normalize_for_fuzzy(&e.old_text)
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

async fn edit_tool_execute_with_operations(
    filesystem: &FilesystemCapability,
    context: &agent_core::api::tool::ToolExecutionContext,
    args: serde_json::Value,
    ops: Arc<dyn EditOperations>,
) -> Result<AgentToolOutput, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("edit: missing or non-string 'path' argument")?
        .to_string();
    let replacements = parse_edits(&args)?
        .into_iter()
        .map(|edit| SelfHealingEditReplacement::new(edit.old_text, edit.new_text))
        .collect::<Vec<_>>();
    let target = filesystem_target_for_execution(filesystem, context, "edit", &path).await?;
    let options =
        SelfHealingEditOptions::from_bound_target(filesystem.clone(), target, path, replacements)
            .with_operations(ops);
    let mut context = SelfHealingEditContext::new(options);
    let runner = SelfHealingEditRunner::new().map_err(|error| error.to_string())?;
    match runner.run_typed(&mut context, None).await {
        Ok(_) => context
            .finish_success()
            .map(self_healing_outcome_to_tool_output)
            .map_err(coding_session_error_message),
        Err(error) => Err(coding_session_error_message(error)),
    }
}

fn coding_session_error_message(error: CodingSessionError) -> String {
    match error {
        CodingSessionError::Config { message }
        | CodingSessionError::Input { message }
        | CodingSessionError::Resource { message }
        | CodingSessionError::Session { message }
        | CodingSessionError::SessionWriteRejected { message }
        | CodingSessionError::SessionWriteFailure { message, .. }
        | CodingSessionError::SelfHealingEditFailed { message, .. }
        | CodingSessionError::Provider { message }
        | CodingSessionError::Tool { message }
        | CodingSessionError::Workflow { message } => message,
        CodingSessionError::Cancelled => "cancelled".to_owned(),
        CodingSessionError::UnsupportedCapability { capability } => {
            format!("unsupported capability: {capability}")
        }
        CodingSessionError::Busy { operation } => format!("busy: {operation}"),
        CodingSessionError::PartialCommit {
            operation_id,
            message,
        } => format!("partial commit uncertainty for operation {operation_id}: {message}"),
        pending @ CodingSessionError::RecoveryPending { .. } => pending.to_string(),
        gap @ CodingSessionError::EventStreamGap { .. } => gap.to_string(),
        lag @ CodingSessionError::EventStreamLag { .. } => lag.to_string(),
        version @ CodingSessionError::UnsupportedProtocolVersion { .. } => version.to_string(),
        other @ (CodingSessionError::SubmissionPreparationBusy
        | CodingSessionError::SubmissionDraftMismatch
        | CodingSessionError::ClientCapacityExceeded { .. }
        | CodingSessionError::Lifecycle { .. }) => other.to_string(),
    }
}

fn self_healing_outcome_to_tool_output(outcome: SelfHealingEditOutcome) -> AgentToolOutput {
    let diagnostics = outcome
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>();
    let check_output = outcome.check_output.as_ref().map(|output| {
        serde_json::json!({
            "command": output.command.clone(),
            "stdout": output.stdout.clone(),
            "stderr": output.stderr.clone(),
            "exitCode": output.exit_code,
        })
    });
    let mut workflow = serde_json::json!({
        "attempts": outcome.attempts,
        "diagnostics": diagnostics,
    });
    if let Some(check_output) = check_output {
        workflow["checkOutput"] = check_output;
    }

    let mut details = serde_json::json!({
        "diff": outcome.diff,
        "patch": outcome.patch,
        "selfHealingEdit": workflow,
    });
    if let Some(first_changed_line) = outcome.first_changed_line {
        details["firstChangedLine"] = serde_json::json!(first_changed_line);
    }

    AgentToolOutput::new(vec![ContentBlock::Text {
        text: outcome.message,
        text_signature: None,
    }])
    .with_details(details)
}

pub(crate) async fn edit_execute_with_target(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn EditOperations>,
) -> Result<AgentToolOutput, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("edit: missing or non-string 'path' argument")?
        .to_string();
    let edits = parse_edits(&args)?;
    let mutation = FileMutation::begin(target).await?;
    let target = target.clone();
    let opened = ops.open_file(&target).await?;
    let raw = opened.read_file().await?;
    let content = String::from_utf8(raw).map_err(|error| {
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
    let mut details = serde_json::json!({
        "diff": diff.diff,
        "patch": patch,
    });
    if let Some(first_changed_line) = diff.first_changed_line {
        details["firstChangedLine"] = serde_json::json!(first_changed_line);
    }
    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
        text: format!("Successfully replaced {} block(s) in {path}.", edits.len()),
        text_signature: None,
    }])
    .with_details(details))
}

pub fn edit_tool(filesystem: FilesystemCapability) -> AgentTool {
    edit_tool_with_operations(filesystem, Arc::new(RealEditOperations))
}

pub fn edit_tool_with_operations(
    filesystem: FilesystemCapability,
    ops: Arc<dyn EditOperations>,
) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        let ops = ops.clone();
        Box::pin(async move {
            edit_tool_execute_with_operations(&filesystem, &context, args, ops).await
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "edit".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Edit, RealEditOperations, apply_edits, edit_execute_with_target};
    use crate::platform::fs::capability::FilesystemCapability;

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
}
