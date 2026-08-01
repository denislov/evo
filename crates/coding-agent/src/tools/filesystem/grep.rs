use crate::runtime::facade::FilesystemCapability;
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::cap_walk::{
    CapWalkEntry, CapWalkEntryKind, CapWalkRoot, walk_target,
};
use crate::tools::filesystem_target_for_execution;
use crate::tools::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use globset::{GlobBuilder, GlobMatcher};
use regex::RegexBuilder;
use std::path::Path;
use std::sync::Arc;

const DESCRIPTION: &str = "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.";
const DEFAULT_LIMIT: usize = 100;
const MAX_LINE_CHARS: usize = 500;
const MAX_GREP_FILE_BYTES: u64 = 5 * 1024 * 1024;

struct Candidate {
    entry: CapWalkEntry,
    display: String,
    basename: String,
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
            "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
            "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.rs' or '**/*.spec.ts'" },
            "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
            "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
            "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
            "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" }
        },
        "required": ["pattern"]
    })
}

fn text_block(text: String) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text,
        text_signature: None,
    }]
}

fn limit_arg(args: &serde_json::Value, default: usize) -> usize {
    args.get("limit")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
                .or_else(|| value.as_f64().map(|n| n.max(1.0) as u64))
        })
        .map(|n| n.max(1) as usize)
        .unwrap_or(default)
}

fn context_arg(args: &serde_json::Value) -> usize {
    args.get("context")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|n| u64::try_from(n.max(0)).ok()))
                .or_else(|| value.as_f64().map(|n| n.max(0.0) as u64))
        })
        .unwrap_or(0) as usize
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

fn output_match_block(
    out: &mut Vec<String>,
    candidate: &Candidate,
    lines: &[&str],
    line_index: usize,
    context: usize,
) -> bool {
    let start = if context == 0 {
        line_index
    } else {
        line_index.saturating_sub(context)
    };
    let end = if context == 0 {
        line_index
    } else {
        (line_index + context).min(lines.len().saturating_sub(1))
    };
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

async fn grep_target(
    target: &FilesystemTarget,
    args: serde_json::Value,
) -> Result<Vec<ContentBlock>, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("grep: missing or non-string 'pattern' argument")?
        .to_owned();
    let glob = args.get("glob").and_then(|v| v.as_str()).map(str::to_owned);
    let ignore_case = args
        .get("ignoreCase")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let literal = args
        .get("literal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let context = context_arg(&args);
    let limit = limit_arg(&args, DEFAULT_LIMIT);
    let target = target.clone();
    tokio::task::spawn_blocking(move || {
        grep_target_blocking(target, pattern, glob, ignore_case, literal, context, limit)
    })
    .await
    .map_err(|error| format!("grep: blocking filesystem task failed: {error}"))?
}

fn grep_target_blocking(
    target: FilesystemTarget,
    pattern: String,
    glob: Option<String>,
    ignore_case: bool,
    literal: bool,
    context: usize,
    limit: usize,
) -> Result<Vec<ContentBlock>, String> {
    let regex_pattern = if literal {
        regex::escape(&pattern)
    } else {
        pattern
    };
    let regex = RegexBuilder::new(&regex_pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| format!("grep: invalid regex: {e}"))?;

    let glob_matcher = glob.as_deref().map(compile_glob).transpose()?;
    let glob_matches_path = glob
        .as_deref()
        .map(|pattern| pattern.contains('/'))
        .unwrap_or(false);
    let mut output_lines = Vec::new();
    let mut match_count = 0usize;
    let mut match_limit_reached = false;
    let mut lines_truncated = false;
    let mut skipped_large_files = 0usize;

    for candidate in candidates_for_walk(walk_target(&target)?) {
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
        return Ok(text_block(message));
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
        notices.push(format!(
            "{limit} matches limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
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

    Ok(text_block(output))
}

pub fn grep_tool(filesystem: FilesystemCapability) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let target =
                filesystem_target_for_execution(&filesystem, &context, "grep", path).await?;
            grep_target(&target, args).await.map(AgentToolOutput::new)
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "grep".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}
