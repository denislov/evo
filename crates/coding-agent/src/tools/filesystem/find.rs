use crate::runtime::facade::FilesystemCapability;
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::cap_walk::{CapWalkEntryKind, CapWalkRoot, walk_target};
use crate::tools::filesystem_target_for_execution;
use crate::tools::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use globset::{GlobBuilder, GlobMatcher};
use std::path::Path;
use std::sync::Arc;

const DESCRIPTION: &str = "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).";
const DEFAULT_LIMIT: usize = 1000;

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern to match files, e.g. '*.rs', '**/*.json', or 'src/**/*.spec.ts'" },
            "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
            "limit": { "type": "number", "description": "Maximum number of results to return (default: 1000)" }
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

fn compile_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| format!("find: invalid glob: {e}"))
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

fn basename(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn sort_paths(paths: &mut [String]) {
    paths.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
}

#[cfg(test)]
pub async fn find_execute(
    cwd: &Path,
    args: serde_json::Value,
) -> Result<Vec<ContentBlock>, String> {
    let filesystem =
        FilesystemCapability::new(cwd.to_path_buf()).map_err(|error| error.to_string())?;
    let requested = args
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    let target = filesystem
        .prepare_target_for_tool("find", requested)
        .await
        .map_err(|error| error.to_string())?;
    find_target(&target, args).await
}

async fn find_target(
    target: &FilesystemTarget,
    args: serde_json::Value,
) -> Result<Vec<ContentBlock>, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("find: missing or non-string 'pattern' argument")?;
    let limit = limit_arg(&args, DEFAULT_LIMIT);
    let matcher = compile_glob(pattern)?;
    let match_path = pattern.contains('/');
    let walked = {
        let target = target.clone();
        tokio::task::spawn_blocking(move || walk_target(&target))
            .await
            .map_err(|error| format!("find: blocking filesystem task failed: {error}"))??
    };
    let entries = match walked {
        CapWalkRoot::Directory(entries) => entries,
        CapWalkRoot::File(_) => {
            return Err(format!(
                "find: not a directory: {}",
                target.display_path().display()
            ));
        }
    };

    let mut matches = Vec::new();
    for entry in entries {
        let Some(relative) = relative_posix(&entry.relative) else {
            continue;
        };
        let target = if match_path {
            relative.clone()
        } else {
            basename(&entry.relative).unwrap_or_default()
        };
        if !matcher.is_match(&target) {
            continue;
        }
        matches.push(if entry.kind == CapWalkEntryKind::Directory {
            format!("{relative}/")
        } else {
            relative
        });
    }

    sort_paths(&mut matches);
    if matches.is_empty() {
        return Ok(text_block("No files found matching pattern".to_string()));
    }

    let result_limit_reached = matches.len() > limit;
    let output = matches
        .into_iter()
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n");
    let truncation = truncate_head(
        &output,
        TruncationLimit {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut output = truncation.content;

    let mut notices = Vec::new();
    if result_limit_reached {
        notices.push(format!(
            "{limit} results limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(text_block(output))
}

pub fn find_tool(filesystem: FilesystemCapability) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let target =
                filesystem_target_for_execution(&filesystem, &context, "find", path).await?;
            find_target(&target, args).await.map(AgentToolOutput::new)
        })
    });
    AgentTool {
        name: "find".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}
