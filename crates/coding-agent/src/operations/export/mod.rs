use std::path::{Path, PathBuf};

use crate::profiles::{ProfileId, ProfileKind};
use crate::runtime::capability::{OperationCapabilitySnapshot, SessionReadCapability};
use crate::runtime::error::CodingSessionError;
use crate::runtime::facade::context::CodingAgentSessionSummary;
use crate::session::event::{DiagnosticLevel, PersistedContentBlock, PersistedDelegationStatus};
use crate::session::replay::{MessageStatus, SessionReplay, ToolCallStatus, TranscriptItem};
use crate::session::service::SessionPersistence;

pub(crate) mod runner;

pub(crate) fn run(
    options: runner::ExportOptions,
    snapshot: &OperationCapabilitySnapshot,
    persistence: &SessionPersistence,
) -> Result<runner::ExportOutcome, CodingSessionError> {
    SessionReadCapability::require(snapshot.session_read.as_ref())?;
    let SessionPersistence::Persistent(session_service) = persistence else {
        return Err(CodingSessionError::UnsupportedCapability {
            capability: "export requires a persistent Rust-native session".into(),
        });
    };
    let mut context = session_service.export_context(options)?;
    runner::ExportRunner::new()?.run_typed(&mut context)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionExport {
    pub summary: CodingAgentSessionSummary,
    pub cwd: Option<String>,
    pub transcript: Vec<CodingAgentSessionExportItem>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentSessionExportItem {
    User {
        text: String,
    },
    Assistant {
        id: String,
        text: String,
        done: bool,
    },
    Tool {
        call_id: String,
        name: String,
        args: serde_json::Value,
        result: Option<String>,
        is_error: bool,
    },
    Delegation {
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: String,
        child_operation_id: Option<String>,
        summary: Option<String>,
    },
    CompactionSummary {
        summary: String,
    },
    BranchSummary {
        summary: String,
    },
    Diagnostic {
        message: String,
    },
}

pub(crate) fn export_from_replay(
    summary: CodingAgentSessionSummary,
    replay: SessionReplay,
) -> CodingAgentSessionExport {
    CodingAgentSessionExport {
        summary,
        cwd: replay.cwd,
        transcript: replay
            .transcript
            .into_iter()
            .map(export_item_from_replay)
            .collect(),
        diagnostics: replay
            .diagnostics
            .into_iter()
            .map(|diagnostic| format_diagnostic(diagnostic.level, &diagnostic.message))
            .collect(),
    }
}

pub(crate) fn write_rendered_export_html(
    html: &str,
    path: &Path,
) -> Result<PathBuf, CodingSessionError> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        return Err(CodingSessionError::Input {
            message: "JSONL session export is no longer supported".into(),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CodingSessionError::Session {
            message: error.to_string(),
        })?;
    }

    std::fs::write(path, html).map_err(|error| CodingSessionError::Session {
        message: error.to_string(),
    })?;
    Ok(path.to_path_buf())
}

fn export_item_from_replay(item: TranscriptItem) -> CodingAgentSessionExportItem {
    match item {
        TranscriptItem::UserInput { text, .. } => CodingAgentSessionExportItem::User { text },
        TranscriptItem::AssistantMessage {
            message_id,
            content,
            status,
        } => CodingAgentSessionExportItem::Assistant {
            id: message_id,
            text: persisted_content_blocks_text(&content),
            done: !matches!(status, MessageStatus::Started),
        },
        TranscriptItem::ToolCall {
            tool_call_id,
            name,
            arguments,
            status,
            summary,
        } => CodingAgentSessionExportItem::Tool {
            call_id: tool_call_id,
            name,
            args: arguments,
            result: if summary.is_empty() {
                None
            } else {
                Some(summary)
            },
            is_error: matches!(status, ToolCallStatus::Failed),
        },
        TranscriptItem::DelegationBlock {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => CodingAgentSessionExportItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status: delegation_status_label(status).into(),
            child_operation_id,
            summary,
        },
        TranscriptItem::CompactionSummary { summary, .. } => {
            CodingAgentSessionExportItem::CompactionSummary { summary }
        }
        TranscriptItem::BranchSummary { summary, .. } => {
            CodingAgentSessionExportItem::BranchSummary { summary }
        }
        TranscriptItem::Diagnostic { message, .. } => {
            CodingAgentSessionExportItem::Diagnostic { message }
        }
    }
}

pub(crate) fn render_export_html(export: &CodingAgentSessionExport) -> String {
    let mut body = String::new();
    if let Some(cwd) = export.cwd.as_deref() {
        body.push_str(&format!(
            "<section class=\"meta\"><h2>Workspace</h2><pre>{}</pre></section>",
            html_escape(cwd)
        ));
    }
    for item in &export.transcript {
        match item {
            CodingAgentSessionExportItem::User { text } => body.push_str(&format!(
                "<section class=\"message user\"><h2>User</h2><pre>{}</pre></section>",
                html_escape(text)
            )),
            CodingAgentSessionExportItem::Assistant { text, done, .. } => {
                let status = if *done { "" } else { " incomplete" };
                body.push_str(&format!(
                    "<section class=\"message assistant{status}\"><h2>Assistant</h2><pre>{}</pre></section>",
                    html_escape(text)
                ));
            }
            CodingAgentSessionExportItem::Tool {
                name,
                args,
                result,
                is_error,
                ..
            } => {
                let args = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
                body.push_str(&format!(
                    "<section class=\"message tool{}\"><h2>Tool: {}</h2><h3>Arguments</h3><pre>{}</pre><h3>Result</h3><pre>{}</pre></section>",
                    if *is_error { " error" } else { "" },
                    html_escape(name),
                    html_escape(&args),
                    html_escape(result.as_deref().unwrap_or(""))
                ));
            }
            CodingAgentSessionExportItem::Delegation {
                target_kind,
                target_id,
                task,
                status,
                summary,
                ..
            } => body.push_str(&format!(
                "<section class=\"message delegation\"><h2>Delegation: {} {}</h2><h3>Status</h3><pre>{}</pre><h3>Task</h3><pre>{}</pre><h3>Summary</h3><pre>{}</pre></section>",
                html_escape(profile_kind_label(*target_kind)),
                html_escape(target_id.as_str()),
                html_escape(status),
                html_escape(task),
                html_escape(summary.as_deref().unwrap_or(""))
            )),
            CodingAgentSessionExportItem::CompactionSummary { summary } => body.push_str(&format!(
                "<section class=\"message compaction\"><h2>Compaction Summary</h2><pre>{}</pre></section>",
                html_escape(summary)
            )),
            CodingAgentSessionExportItem::BranchSummary { summary } => body.push_str(&format!(
                "<section class=\"message branch-summary\"><h2>Branch Summary</h2><pre>{}</pre></section>",
                html_escape(summary)
            )),
            CodingAgentSessionExportItem::Diagnostic { message } => body.push_str(&format!(
                "<section class=\"message diagnostic\"><h2>Diagnostic</h2><pre>{}</pre></section>",
                html_escape(message)
            )),
        }
    }
    for diagnostic in &export.diagnostics {
        body.push_str(&format!(
            "<section class=\"message diagnostic\"><h2>Diagnostic</h2><pre>{}</pre></section>",
            html_escape(diagnostic)
        ));
    }

    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}</style></head><body><main><h1>{}</h1>{}</main></body></html>",
        html_escape(&export.summary.session_id),
        "body{font-family:system-ui,sans-serif;margin:2rem;background:#101010;color:#f4f4f4}main{max-width:900px;margin:auto}.meta,.message{border:1px solid #444;padding:1rem;margin:1rem 0;border-radius:6px}pre{white-space:pre-wrap;font-family:ui-monospace,monospace}.user{border-color:#3b82f6}.assistant{border-color:#10b981}.tool{border-color:#a78bfa}.delegation{border-color:#38bdf8}.error{border-color:#ef4444;color:#fecaca}.diagnostic{border-color:#f59e0b}.compaction{border-color:#14b8a6}.branch-summary{border-color:#f97316}.incomplete{opacity:.75}",
        html_escape(&export.summary.session_id),
        body
    )
}

fn persisted_content_blocks_text(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => text.clone(),
            PersistedContentBlock::Thinking { thinking, .. } => thinking.clone(),
            PersistedContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_diagnostic(level: DiagnosticLevel, message: &str) -> String {
    format!("{level:?}: {message}")
}

fn delegation_status_label(status: PersistedDelegationStatus) -> &'static str {
    match status {
        PersistedDelegationStatus::Requested => "requested",
        PersistedDelegationStatus::Running => "running",
        PersistedDelegationStatus::Completed => "completed",
        PersistedDelegationStatus::Failed => "failed",
        PersistedDelegationStatus::Rejected => "rejected",
        PersistedDelegationStatus::Cancelled => "cancelled",
        PersistedDelegationStatus::ConfirmationRequired => "confirmation_required",
    }
}

fn profile_kind_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
