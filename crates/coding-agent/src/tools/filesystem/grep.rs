use crate::platform::io::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem_target_for_runtime_execution;
use globset::{GlobBuilder, GlobMatcher};
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use std::path::Path;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};
use workspace_runtime::api::WorkspaceAccessHandle;
use workspace_runtime::api::{CapWalkEntry, CapWalkEntryKind, CapWalkRoot, walk_target};

const DESCRIPTION: &str = "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1_000;
const MAX_CONTEXT: usize = 20;
const MAX_LINE_CHARS: usize = 500;
const MAX_GREP_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    /// Search pattern (regex unless `literal` is true).
    pattern: String,
    /// Directory or file to search (default: current directory).
    #[serde(default = "default_path")]
    path: String,
    /// Optional file glob filter, such as `*.rs` or `**/*.spec.ts`.
    #[serde(default)]
    glob: Option<String>,
    /// Case-insensitive search.
    #[serde(default, rename = "ignoreCase")]
    #[schemars(rename = "ignoreCase")]
    ignore_case: bool,
    /// Treat pattern as a literal string instead of a regular expression.
    #[serde(default)]
    literal: bool,
    /// Number of context lines before and after each match.
    #[schemars(range(min = 0, max = 20))]
    #[serde(default, deserialize_with = "deserialize_optional_context")]
    context: Option<u64>,
    /// Maximum number of matching lines to return.
    #[schemars(range(min = 1, max = 1_000))]
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    limit: Option<u64>,
}

fn default_path() -> String {
    ".".into()
}

fn deserialize_optional_context<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value > MAX_CONTEXT as u64) {
        return Err(serde::de::Error::custom(format!(
            "context must be between 0 and {MAX_CONTEXT}"
        )));
    }
    Ok(value)
}

fn deserialize_optional_limit<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value == 0 || value > MAX_LIMIT as u64) {
        return Err(serde::de::Error::custom(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(value)
}

impl GrepArgs {
    fn context(&self) -> Result<usize, ToolError> {
        let context = self.context.unwrap_or(0);
        if context > MAX_CONTEXT as u64 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("grep: context must be between 0 and {MAX_CONTEXT}"),
            ));
        }
        Ok(context as usize)
    }

    fn limit(&self) -> Result<usize, ToolError> {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT as u64);
        if limit == 0 || limit > MAX_LIMIT as u64 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("grep: limit must be between 1 and {MAX_LIMIT}"),
            ));
        }
        Ok(limit as usize)
    }
}

struct Candidate {
    entry: CapWalkEntry,
    display: String,
    basename: String,
}

fn text_block(text: String) -> Vec<ToolContent> {
    vec![ToolContent::Text { text }]
}

fn compile_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| format!("grep: invalid glob: {e}"))
}

fn relative_posix(relative: &Path) -> Option<String> {
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(
        relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        a.display
            .to_lowercase()
            .cmp(&b.display.to_lowercase())
            .then_with(|| a.display.cmp(&b.display))
    });
}

fn candidates_for_walk(walked: CapWalkRoot) -> Vec<Candidate> {
    let entries = match walked {
        CapWalkRoot::File(entry) => vec![entry],
        CapWalkRoot::Directory(entries) => entries,
    };
    let mut candidates = Vec::new();
    for entry in entries {
        if entry.kind != CapWalkEntryKind::File {
            continue;
        }
        let Some(display) = relative_posix(&entry.relative) else {
            continue;
        };
        let basename = basename(&entry.relative);
        candidates.push(Candidate {
            entry,
            display,
            basename,
        });
    }
    sort_candidates(&mut candidates);
    candidates
}

fn truncate_line(line: &str) -> (String, bool) {
    let truncated = line.chars().nth(MAX_LINE_CHARS).is_some();
    if !truncated {
        return (line.to_string(), false);
    }
    (line.chars().take(MAX_LINE_CHARS).collect(), true)
}

fn context_window(line_index: usize, line_count: usize, context: usize) -> (usize, usize) {
    let last_line = line_count.saturating_sub(1);
    (
        line_index.saturating_sub(context),
        line_index.saturating_add(context).min(last_line),
    )
}

fn output_match_block(
    out: &mut Vec<String>,
    candidate: &Candidate,
    lines: &[&str],
    line_index: usize,
    context: usize,
) -> bool {
    let (start, end) = context_window(line_index, lines.len(), context);
    let mut any_truncated = false;
    for current in start..=end {
        let (line, truncated) = truncate_line(lines.get(current).copied().unwrap_or_default());
        any_truncated |= truncated;
        let line_number = current + 1;
        if current == line_index {
            out.push(format!("{}:{line_number}: {line}", candidate.display));
        } else {
            out.push(format!("{}-{line_number}- {line}", candidate.display));
        }
    }
    any_truncated
}

async fn grep_target(target: &FilesystemTarget, args: GrepArgs) -> Result<ToolOutput, ToolError> {
    let context = args.context()?;
    let limit = args.limit()?;
    let pattern = args.pattern;
    let path = args.path;
    let glob = args.glob;
    let ignore_case = args.ignore_case;
    let literal = args.literal;
    let target_fingerprint = target.target_fingerprint().to_owned();
    let target = target.clone();
    tokio::task::spawn_blocking(move || {
        grep_target_blocking(GrepExecution {
            target,
            path,
            pattern,
            glob,
            ignore_case,
            literal,
            context,
            limit,
            target_fingerprint,
        })
    })
    .await
    .map_err(|error| {
        ToolError::new(
            ToolErrorKind::Execution,
            format!("grep: blocking filesystem task failed: {error}"),
        )
    })?
}

struct GrepExecution {
    target: FilesystemTarget,
    path: String,
    pattern: String,
    glob: Option<String>,
    ignore_case: bool,
    literal: bool,
    context: usize,
    limit: usize,
    target_fingerprint: String,
}

fn grep_target_blocking(input: GrepExecution) -> Result<ToolOutput, ToolError> {
    let GrepExecution {
        target,
        path,
        pattern,
        glob,
        ignore_case,
        literal,
        context,
        limit,
        target_fingerprint,
    } = input;
    let regex_pattern = if literal {
        regex::escape(&pattern)
    } else {
        pattern.clone()
    };
    let regex = RegexBuilder::new(&regex_pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| {
            ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("grep: invalid regex: {e}"),
            )
        })?;

    let glob_matcher = glob
        .as_deref()
        .map(compile_glob)
        .transpose()
        .map_err(|error| ToolError::new(ToolErrorKind::InvalidArguments, error))?;
    let glob_matches_path = glob
        .as_deref()
        .map(|pattern| pattern.contains('/'))
        .unwrap_or(false);
    let mut output_lines = Vec::new();
    let mut match_count = 0usize;
    let mut match_limit_reached = false;
    let mut lines_truncated = false;
    let mut skipped_large_files = 0usize;

    let walked = walk_target(&target)
        .map_err(|error| ToolError::new(ToolErrorKind::Execution, error.to_string()))?;
    for candidate in candidates_for_walk(walked) {
        if let Some(matcher) = &glob_matcher {
            let target = if glob_matches_path {
                candidate.display.as_str()
            } else {
                candidate.basename.as_str()
            };
            if !matcher.is_match(target) {
                continue;
            }
        }

        let raw = match candidate.entry.read_bounded(MAX_GREP_FILE_BYTES) {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                skipped_large_files += 1;
                continue;
            }
            Err(_) => continue,
        };
        let content = String::from_utf8_lossy(&raw)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let lines = content.split('\n').collect::<Vec<_>>();
        for (line_index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            match_count += 1;
            lines_truncated |=
                output_match_block(&mut output_lines, &candidate, &lines, line_index, context);
            if match_count >= limit {
                match_limit_reached = true;
                break;
            }
        }
        if match_limit_reached {
            break;
        }
    }

    if match_count == 0 {
        let mut message = "No matches found".to_string();
        if skipped_large_files > 0 {
            message.push_str(&format!(
                "\n\n[{skipped_large_files} file(s) skipped because they exceeded the {} safety limit]",
                format_size(MAX_GREP_FILE_BYTES as usize)
            ));
        }
        return Ok(ToolOutput {
            content: text_block(message),
            details: Some(serde_json::json!({
                "path": path,
                "pattern": pattern,
                "target_fingerprint": target_fingerprint,
                "matches": 0,
                "skipped_large_files": skipped_large_files,
                "truncated": false,
            })),
            terminate: false,
        });
    }

    let output = output_lines.join("\n");
    let truncation = truncate_head(
        &output,
        TruncationLimit {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut output = truncation.content;

    let mut notices = Vec::new();
    if match_limit_reached {
        let suggested = limit.saturating_mul(2).min(MAX_LIMIT);
        notices.push(if suggested > limit {
            format!(
                "{limit} matches limit reached. Use limit={suggested} for more, or refine pattern"
            )
        } else {
            format!("Maximum {MAX_LIMIT} matches reached. Refine pattern for a narrower result")
        });
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {MAX_LINE_CHARS} chars. Use read tool to see full lines"
        ));
    }
    if skipped_large_files > 0 {
        notices.push(format!(
            "{skipped_large_files} file(s) skipped because they exceeded the {} safety limit",
            format_size(MAX_GREP_FILE_BYTES as usize)
        ));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(ToolOutput {
        content: text_block(output),
        details: Some(serde_json::json!({
            "path": path,
            "pattern": pattern,
            "target_fingerprint": target_fingerprint,
            "matches": match_count,
            "skipped_large_files": skipped_large_files,
            "truncated": match_limit_reached || truncation.truncated || lines_truncated,
        })),
        terminate: false,
    })
}

pub fn grep_runtime_tool(
    filesystem: WorkspaceAccessHandle,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("grep").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<GrepArgs>().expect("GrepArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: true,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::WorkspaceLocalReadOnly,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<GrepArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            Box::pin(async move {
                let target = filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "grep",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                grep_target(&target, args).await
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    fn runtime(filesystem: WorkspaceAccessHandle) -> ToolRuntime {
        let mut registry = ToolRegistry::default();
        registry
            .register(grep_runtime_tool(filesystem).unwrap())
            .unwrap();
        ToolRuntime::new(registry).unwrap()
    }

    fn context() -> ToolCallContext {
        ToolCallContext::new(
            ToolId::new("grep").unwrap(),
            "grep-call",
            CancellationToken::new(),
        )
    }

    #[test]
    fn schema_and_runtime_share_the_context_maximum() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let tool = grep_runtime_tool(filesystem).unwrap();
        let definition = tool.definition();
        assert_eq!(definition.parameters["additionalProperties"], false);
        assert_eq!(definition.parameters["required"], json!(["pattern"]));
        assert_eq!(
            definition.parameters["properties"]["context"]["anyOf"][0]["maximum"],
            json!(MAX_CONTEXT)
        );
        assert_eq!(
            definition.parameters["properties"]["limit"]["anyOf"][0]["maximum"],
            json!(MAX_LIMIT)
        );
    }

    #[test]
    fn context_window_saturates_at_both_ends() {
        assert_eq!(context_window(1, 3, usize::MAX), (0, 2));
        assert_eq!(context_window(5, 10, 2), (3, 7));
    }

    #[tokio::test]
    async fn invalid_context_types_are_explicit_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let runtime = runtime(filesystem);
        for value in [json!(-1), json!(21), json!(1.5), json!("1")] {
            let error = runtime
                .execute(context(), json!({"pattern": "needle", "context": value}))
                .await
                .expect_err("invalid context must fail");
            assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
        }
    }

    #[tokio::test]
    async fn typed_grep_searches_with_context_and_returns_bounded_details() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("notes.txt"),
            "before\nNeedle\nafter\nother\n",
        )
        .expect("write fixture");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let output = runtime(filesystem)
            .execute(
                context(),
                json!({"pattern": "needle", "ignoreCase": true, "context": 1}),
            )
            .await
            .expect("typed grep succeeds");
        assert!(matches!(
            output.content.as_slice(),
            [ToolContent::Text { text }] if text == "notes.txt-1- before\nnotes.txt:2: Needle\nnotes.txt-3- after"
        ));
        let details = output.details.expect("grep details");
        assert_eq!(details["matches"], 1);
        assert_eq!(details["skipped_large_files"], 0);
        assert_eq!(details["truncated"], false);
        assert!(
            details["target_fingerprint"]
                .as_str()
                .is_some_and(|value| value.len() == 64)
        );
    }
}
