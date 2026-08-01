use crate::interactive::error::CliError;
use crate::interactive::transcript::TranscriptItem;
use coding_agent::api::embedding::CodingAgentSessionQuery;
use coding_agent::api::runtime::CodingAgentSessionBootstrap;
use coding_agent::api::view::{
    CodingAgentSessionChoice, CodingAgentSessionChoiceKind, CodingAgentSessionSnapshot,
    CodingAgentSessionTranscriptItem, CodingAgentSessionTreeNode,
};
use std::path::{Path, PathBuf};

pub(super) type SessionChoice = CodingAgentSessionChoice;
pub(super) type SessionChoiceKind = CodingAgentSessionChoiceKind;

/// Cumulative usage statistics computed from all assistant messages in a
/// hydrated session.  Used to initialise [`super::root::FooterStats`] so the
/// footer shows correct token/cost numbers immediately after resume, without
/// waiting for the next turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct CumulativeUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub cost: f64,
    /// Context-token estimate from the *last* assistant message with a usage
    /// block.  `None` means no assistant message has reported usage yet.
    pub last_context_tokens: Option<u32>,
}

pub(super) struct HydratedSession {
    pub(super) choice: SessionChoice,
    pub(super) transcript_items: Vec<TranscriptItem>,
    pub(super) leaf_id: Option<String>,
    pub(super) cumulative_usage: CumulativeUsage,
}

pub(super) fn hydrate_existing_session_target(
    bootstrap: &CodingAgentSessionBootstrap,
) -> Result<Option<HydratedSession>, CliError> {
    Ok(bootstrap
        .selected_snapshot()?
        .map(hydrated_session_from_snapshot))
}

pub(super) fn hydrated_session_from_snapshot(
    snapshot: CodingAgentSessionSnapshot,
) -> HydratedSession {
    let leaf_id = snapshot.choice.active_leaf_id.clone();
    HydratedSession {
        choice: snapshot.choice,
        transcript_items: snapshot
            .transcript
            .into_iter()
            .flat_map(transcript_items_from_rust_native)
            .collect(),
        leaf_id,
        cumulative_usage: CumulativeUsage {
            input: snapshot.usage.input,
            output: snapshot.usage.output,
            cache_read: snapshot.usage.cache_read,
            cache_write: snapshot.usage.cache_write,
            cost: snapshot.usage.cost,
            last_context_tokens: snapshot.usage.last_context_tokens,
        },
    }
}

pub(super) fn clone_rust_native_choice(
    query: &CodingAgentSessionQuery,
    choice: &SessionChoice,
) -> Result<HydratedSession, CliError> {
    if choice.kind != SessionChoiceKind::Persistent {
        return Err(CliError::SessionFailure(
            "session choice is not persistent".into(),
        ));
    }
    query
        .clone_session(&choice.id)
        .map(hydrated_session_from_snapshot)
        .map_err(CliError::from)
}

pub(super) fn rust_native_tree_for_choice(
    query: &CodingAgentSessionQuery,
    choice: &SessionChoice,
) -> Result<(Vec<CodingAgentSessionTreeNode>, Option<String>), CliError> {
    let tree = query.tree(&choice.id)?;
    Ok((tree.roots, tree.active_leaf_id))
}

fn transcript_items_from_rust_native(
    item: CodingAgentSessionTranscriptItem,
) -> Vec<TranscriptItem> {
    let item = match item {
        CodingAgentSessionTranscriptItem::User { text, .. } => TranscriptItem::user(text),
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
            ..
        } => {
            let mut items = Vec::with_capacity(1 + images.len());
            if !text.trim().is_empty() || !thinking.trim().is_empty() {
                items.push(TranscriptItem::Assistant {
                    id,
                    markdown: text,
                    thinking,
                    done,
                });
            }
            items.extend(images.into_iter().map(|image| TranscriptItem::Image {
                mime_type: image.mime_type,
                data: image.data,
            }));
            return items;
        }
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
            ..
        } => TranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
        },
        CodingAgentSessionTranscriptItem::CompactionSummary { summary } => {
            TranscriptItem::assistant("compaction", summary, true)
        }
        CodingAgentSessionTranscriptItem::BranchSummary { summary } => {
            TranscriptItem::assistant("branch_summary", summary, true)
        }
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
            ..
        } => {
            let target_kind = match target_kind {
                coding_agent::api::view::ProfileKind::Agent => "agent",
                coding_agent::api::view::ProfileKind::Team => "team",
            };
            TranscriptItem::Tool {
                call_id: tool_call_id,
                name: "delegation".into(),
                args: serde_json::json!({
                    "targetKind": target_kind,
                    "targetId": target_id.as_str(),
                    "task": task,
                    "status": status,
                    "childOperationId": child_operation_id,
                }),
                result: summary.filter(|summary| !summary.trim().is_empty()),
                is_error: status == "failed",
            }
        }
        CodingAgentSessionTranscriptItem::Diagnostic { message } => TranscriptItem::system(message),
    };
    vec![item]
}

pub(super) fn export_path_arg(args: &str) -> Option<String> {
    let args = args.trim_start();
    if args.is_empty() {
        return None;
    }

    let first = args.chars().next()?;
    if first == '"' || first == '\'' {
        let closing = args[1..].find(first)?;
        return Some(args[1..1 + closing].to_string());
    }

    let end = args.find(char::is_whitespace).unwrap_or(args.len());
    Some(args[..end].to_string())
}

pub(super) fn default_export_path(cwd: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    cwd.join(format!("session-{stamp}.html"))
}

pub(super) fn export_rust_native_choice(
    query: &CodingAgentSessionQuery,
    choice: &SessionChoice,
    cwd: &Path,
    args: &str,
) -> Result<PathBuf, String> {
    if choice.kind != SessionChoiceKind::Persistent {
        return Err("session choice is not persistent".into());
    }
    let path = resolve_export_path(cwd, args);
    query
        .export_html(&choice.id, &path)
        .map_err(|error| error.to_string())
}

pub(super) fn export_transcript(
    cwd: &Path,
    session_label: &str,
    model_id: &str,
    items: &[TranscriptItem],
    args: &str,
) -> Result<PathBuf, String> {
    let path = resolve_export_path(cwd, args);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let _ = model_id;
    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        return Err("JSONL session export is no longer supported".to_string());
    }
    export_transcript_html(session_label, items, &path)?;
    Ok(path)
}

fn resolve_export_path(cwd: &Path, args: &str) -> PathBuf {
    let path = export_path_arg(args)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_export_path(cwd));
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn export_transcript_html(
    session_label: &str,
    items: &[TranscriptItem],
    path: &Path,
) -> Result<(), String> {
    let mut body = String::new();
    for item in items {
        match item {
            TranscriptItem::User { text } => body.push_str(&format!(
                "<section class=\"message user\"><h2>User</h2><pre>{}</pre></section>",
                html_escape(text)
            )),
            TranscriptItem::Assistant { markdown, .. } => body.push_str(&format!(
                "<section class=\"message assistant\"><h2>Assistant</h2><pre>{}</pre></section>",
                html_escape(markdown)
            )),
            TranscriptItem::Tool {
                name,
                result,
                is_error,
                ..
            } => body.push_str(&format!(
                "<section class=\"message tool{}\"><h2>Tool: {}</h2><pre>{}</pre></section>",
                if *is_error { " error" } else { "" },
                html_escape(name),
                html_escape(result.as_deref().unwrap_or(""))
            )),
            TranscriptItem::Image { mime_type, data } => body.push_str(&format!(
                "<section class=\"message assistant image\"><h2>Assistant image</h2><img alt=\"Assistant image\" src=\"data:{};base64,{}\"></section>",
                html_escape(mime_type),
                html_escape(data),
            )),
            TranscriptItem::Error { text } => body.push_str(&format!(
                "<section class=\"message error\"><h2>Error</h2><pre>{}</pre></section>",
                html_escape(text)
            )),
            TranscriptItem::System { .. } => {}
        }
    }

    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}</style></head><body><main><h1>{}</h1>{}</main></body></html>",
        html_escape(session_label),
        "body{font-family:system-ui,sans-serif;margin:2rem;background:#101010;color:#f4f4f4}main{max-width:900px;margin:auto}.message{border:1px solid #444;padding:1rem;margin:1rem 0;border-radius:6px}pre{white-space:pre-wrap;font-family:ui-monospace,monospace}img{max-width:100%;height:auto}.user{border-color:#3b82f6}.assistant{border-color:#10b981}.tool{border-color:#a78bfa}.error{border-color:#ef4444;color:#fecaca}",
        html_escape(session_label),
        body
    );
    std::fs::write(path, html).map_err(|error| error.to_string())
}

fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
