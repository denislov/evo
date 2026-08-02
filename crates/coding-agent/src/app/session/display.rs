use super::*;

pub(super) fn session_tree_entry_display_text(
    node: &SessionTreeNode,
    tool_calls: &BTreeMap<String, SessionTreeToolCall>,
) -> String {
    let entry = &node.entry;
    match entry.entry_type.as_str() {
        "message" => session_tree_message_display_text(entry, tool_calls),
        "bashExecution" => {
            let command = entry
                .field("command")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let output = entry
                .field("output")
                .and_then(|value| value.as_str())
                .and_then(|output| output.lines().next())
                .map(|output| normalized_preview(output, 40))
                .unwrap_or_default();
            normalized_preview(
                &format!("[bash] {command} {output}"),
                MAX_SESSION_TREE_PREVIEW_CHARS,
            )
        }
        "toolResult" => {
            let name = entry
                .field("toolName")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let preview = entry
                .field("content")
                .and_then(|value| value.as_array())
                .and_then(|content| content.first())
                .and_then(|block| block.get("text"))
                .and_then(|value| value.as_str())
                .map(|text| normalized_preview(text, 40))
                .unwrap_or_default();
            format!("[toolResult] {name}: {preview}")
        }
        "compaction" => {
            let summary = entry
                .field("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("compacted");
            let tokens = entry
                .field("tokensBefore")
                .and_then(serde_json::Value::as_u64)
                .map(|tokens| (tokens as f64 / 1000.0).round() as u64)
                .unwrap_or(0);
            if tokens > 0 {
                format!("[compaction: {tokens}k tokens]")
            } else {
                format!(
                    "[compaction] {}",
                    normalized_preview(summary, MAX_SESSION_TREE_PREVIEW_CHARS)
                )
            }
        }
        "branch_summary" => {
            let summary = entry
                .field("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("branch");
            format!(
                "[branch summary]: {}",
                normalized_preview(summary, MAX_SESSION_TREE_PREVIEW_CHARS)
            )
        }
        "custom_message" | "custom" => {
            let custom_type = entry
                .field("customType")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[custom: {custom_type}]")
        }
        "session_info" => {
            let name = entry
                .field("name")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!(
                "[title: {}]",
                normalized_preview(name, MAX_SESSION_TREE_PREVIEW_CHARS)
            )
        }
        "model_change" => {
            let model = entry
                .field("modelId")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[model: {model}]")
        }
        "thinking_level_change" => {
            let level = entry
                .field("thinkingLevel")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[thinking: {level}]")
        }
        _ => normalized_preview(
            &format!("[{}] {}", entry.entry_type, entry.id),
            MAX_SESSION_TREE_PREVIEW_CHARS,
        ),
    }
}

pub(super) fn session_tree_message_display_text(
    entry: &SessionEntry,
    tool_calls: &BTreeMap<String, SessionTreeToolCall>,
) -> String {
    let Some(message) = entry.field("message") else {
        return entry.id.clone();
    };
    let Some(role) = message.get("role").and_then(|value| value.as_str()) else {
        return entry.id.clone();
    };
    let preview = session_tree_message_text_preview(message);
    match role {
        "user" => format!("user: {preview}"),
        "assistant" if !preview.is_empty() => format!("assistant: {preview}"),
        "assistant"
            if message.get("stopReason").and_then(|value| value.as_str()) == Some("aborted") =>
        {
            "assistant: (aborted)".to_owned()
        }
        "assistant" => message
            .get("errorMessage")
            .and_then(|value| value.as_str())
            .map(|error| {
                format!(
                    "assistant: {}",
                    normalized_preview(error, MAX_SESSION_TREE_PREVIEW_CHARS)
                )
            })
            .unwrap_or_else(|| "assistant: (no content)".to_owned()),
        "toolResult" => {
            let tool_call = message
                .get("toolCallId")
                .and_then(|value| value.as_str())
                .and_then(|id| tool_calls.get(id));
            if let Some(tool_call) = tool_call {
                format_session_tree_tool_call(&tool_call.name, &tool_call.arguments)
            } else {
                let name = message
                    .get("toolName")
                    .and_then(|value| value.as_str())
                    .unwrap_or("tool");
                format!("[{name}]")
            }
        }
        _ => normalized_preview(
            &format!("[{role}] {preview}"),
            MAX_SESSION_TREE_PREVIEW_CHARS,
        ),
    }
}

pub(super) fn session_tree_message_text_preview(message: &serde_json::Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        serde_json::Value::String(text) => normalized_preview(text, MAX_SESSION_TREE_PREVIEW_CHARS),
        serde_json::Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if block.get("type").and_then(|value| value.as_str()) == Some("text")
                    && let Some(part) = block.get("text").and_then(|value| value.as_str())
                {
                    text.push_str(part);
                    if text.chars().count() >= MAX_SESSION_TREE_PREVIEW_CHARS {
                        break;
                    }
                }
            }
            normalized_preview(&text, MAX_SESSION_TREE_PREVIEW_CHARS)
        }
        _ => String::new(),
    }
}

pub(super) fn normalized_preview(text: &str, max_chars: usize) -> String {
    let normalized = text.replace(['\n', '\t'], " ").trim().to_owned();
    if normalized.chars().count() > max_chars {
        normalized.chars().take(max_chars).collect()
    } else {
        normalized
    }
}

pub(super) fn format_session_tree_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    let argument = |key: &str| arguments.get(key).and_then(|value| value.as_str());
    match name {
        "read" => {
            let path = shorten_session_tree_home(
                argument("path")
                    .or_else(|| argument("file_path"))
                    .unwrap_or(""),
            );
            let mut display = path;
            let offset = arguments.get("offset").and_then(serde_json::Value::as_i64);
            let limit = arguments.get("limit").and_then(serde_json::Value::as_i64);
            if offset.is_some() || limit.is_some() {
                let start = offset.unwrap_or(1);
                display.push(':');
                display.push_str(&start.to_string());
                if let Some(end) = limit.map(|limit| start + limit - 1) {
                    display.push('-');
                    display.push_str(&end.to_string());
                }
            }
            normalized_preview(
                &format!("[read: {display}]"),
                MAX_SESSION_TREE_PREVIEW_CHARS,
            )
        }
        "write" | "edit" => {
            let path = shorten_session_tree_home(
                argument("path")
                    .or_else(|| argument("file_path"))
                    .unwrap_or(""),
            );
            format!("[{name}: {path}]")
        }
        "bash" => {
            let raw = argument("command").unwrap_or("");
            let command = normalized_preview(raw, 50);
            let suffix = if raw.chars().count() > 50 { "..." } else { "" };
            format!("[bash: {command}{suffix}]")
        }
        "grep" | "find" => {
            let pattern = argument("pattern").unwrap_or("");
            let path = shorten_session_tree_home(argument("path").unwrap_or("."));
            let separator = if name == "grep" { "/" } else { "" };
            format!("[{name}: {separator}{pattern}{separator} in {path}]")
        }
        "ls" => {
            let path = shorten_session_tree_home(argument("path").unwrap_or("."));
            format!("[ls: {path}]")
        }
        _ => {
            let arguments = arguments.to_string();
            let preview = normalized_preview(&arguments, 40);
            let suffix = if arguments.chars().count() > 40 {
                "..."
            } else {
                ""
            };
            format!("[{name}: {preview}{suffix}]")
        }
    }
}

pub(super) fn shorten_session_tree_home(path: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_owned()
    }
}
