use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::io::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use crate::tools::FilesystemTarget;
use crate::tools::args::bounded_arg;
use crate::tools::filesystem_target_for_execution;
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use std::sync::Arc;

const DESCRIPTION: &str = "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).";
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 5_000;

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Directory to list (default: current directory)" },
            "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Maximum number of entries to return (default: 500)" }
        }
    })
}

fn text_block(text: String) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text,
        text_signature: None,
    }]
}

fn limit_arg(args: &serde_json::Value) -> Result<usize, String> {
    bounded_arg(args, "limit", DEFAULT_LIMIT, MAX_LIMIT)
        .map(|limit| limit.max(1))
        .map_err(|error| format!("ls: {error}"))
}

async fn ls_target(
    target: &FilesystemTarget,
    args: serde_json::Value,
) -> Result<Vec<ContentBlock>, String> {
    let limit = limit_arg(&args)?;
    let target = target.clone();
    let mut entries = tokio::task::spawn_blocking(move || {
        let directory = target.opened_directory()?;
        let read_dir = directory.entries().map_err(|error| {
            format!(
                "ls: cannot read directory {}: {error}",
                target.display_path().display()
            )
        })?;
        let mut entries = Vec::new();
        for result in read_dir {
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            entries.push(if file_type.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
        Ok::<_, String>(entries)
    })
    .await
    .map_err(|error| format!("ls: blocking filesystem task failed: {error}"))??;

    entries.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });

    if entries.is_empty() {
        return Ok(text_block("(empty directory)".to_string()));
    }

    let entry_limit_reached = entries.len() > limit;
    let output = entries
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
    if entry_limit_reached {
        let suggested = limit.saturating_mul(2).min(MAX_LIMIT);
        notices.push(if suggested > limit {
            format!("{limit} entries limit reached. Use limit={suggested} for more")
        } else {
            format!("Maximum {MAX_LIMIT} entries reached")
        });
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(text_block(output))
}

pub fn ls_tool(filesystem: FilesystemCapability) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let target = filesystem_target_for_execution(&filesystem, &context, "ls", path).await?;
            ls_target(&target, args).await.map(AgentToolOutput::new)
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "ls".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}
