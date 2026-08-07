//! Bounded runtime DTOs, validation, limits, error projection, and kind labels.

mod commands;
mod errors;
mod snapshots;

pub(super) use commands::DesktopRuntimeCommand;
pub use commands::{DesktopPromptTarget, DesktopRuntimeCommandKind, DesktopRuntimeOwnerTarget};
pub(super) use errors::{
    DesktopBridgeError, DesktopRuntimeErrorSource, local_runtime_error, runtime_error,
};
pub use errors::{
    DesktopCommandAdmissionError, DesktopRuntimeError, DesktopRuntimeShutdownError,
    DesktopRuntimeStartError,
};
pub use snapshots::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeHydratedSnapshot,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeReadySnapshot, DesktopRuntimeRecoverySnapshot,
    DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind, DesktopSessionCatalogEntry,
};

use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::client::{CodingAgentControlReceipt, CodingAgentSnapshot};
use coding_agent::api::embedding::CodingAgentThinkingLevel;
use coding_agent::api::embedding::{CodingAgentWorkspaceSelection, global_config_directory};
use coding_agent::api::event::{CodingAgentMergeProposal, CodingAgentProductEvent};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use std::path::{Path, PathBuf};

pub const DESKTOP_COMMAND_QUEUE_CAPACITY: usize = 64;
pub const DESKTOP_UPDATE_QUEUE_CAPACITY: usize = 128;
pub const DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY: usize = 64;
pub const MAX_CONCURRENT_DESKTOP_SESSIONS: usize = 4;
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;
pub const MAX_PROMPT_ATTACHMENTS: usize = 16;
pub const MAX_CONTROL_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_SESSION_NAME_BYTES: usize = 1024;
pub const MAX_DESKTOP_SESSION_CATALOG: usize = 128;
pub const MAX_WORKSPACE_PATH_BYTES: usize = 16 * 1024;

pub(super) const MAX_SESSION_ID_BYTES: usize = 256;
pub(super) const MAX_AUTHORIZATION_ID_BYTES: usize = 256;
pub(super) const MAX_SELECTION_ID_BYTES: usize = 256;
pub(super) const MAX_RECOVERY_ID_BYTES: usize = 1024;
const MAX_FILE_REVIEW_ID_BYTES: usize = 1024;
pub(super) const MAX_FILE_REVIEW_PATH_BYTES: usize = 16 * 1024;
pub(super) const MAX_PROMPT_ATTACHMENT_PATH_BYTES: usize = 16 * 1024;
pub(super) const MAX_PROMPT_ATTACHMENT_PATH_TOTAL_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone)]
pub enum DesktopRuntimeUpdate {
    Reloaded {
        command_id: u64,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    Resynced {
        command_id: u64,
        replacement: DesktopRuntimeResyncSnapshot,
    },
    SessionChanged {
        command_id: u64,
        snapshot: DesktopRuntimeHydratedSnapshot,
    },
    SessionClosed {
        command_id: u64,
        session_id: String,
    },
    SessionDeleted {
        command_id: u64,
        session_id: String,
    },
    SessionsListed {
        command_id: u64,
        sessions: Vec<DesktopSessionCatalogEntry>,
        omitted: usize,
    },
    SessionRenamed {
        command_id: u64,
        session_id: String,
        name: Option<String>,
        updated_at: String,
    },
    SessionNameObserved {
        session_id: String,
        name: Option<String>,
        updated_at: String,
    },
    SelectionChanged {
        command_id: u64,
        selection: DesktopRuntimeSelectionKind,
        thinking_level: Option<CodingAgentThinkingLevel>,
        thinking_fallback: bool,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    PromptAccepted {
        command_id: u64,
    },
    PromptAcceptedWithSession {
        command_id: u64,
        snapshot: DesktopRuntimeHydratedSnapshot,
    },
    PromptRejectedWithSession {
        command_id: u64,
        snapshot: DesktopRuntimeHydratedSnapshot,
        error: DesktopRuntimeError,
    },
    PromptStarted {
        command_id: u64,
        operation_id: String,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    ProductEvent {
        session_id: String,
        event: CodingAgentProductEvent,
    },
    ResyncRequired {
        reason: DesktopRuntimeError,
        snapshot: CodingAgentSnapshot,
    },
    ControlAccepted {
        command_id: u64,
        command: DesktopRuntimeCommandKind,
        receipt: CodingAgentControlReceipt,
    },
    AuthorizationDecisionAccepted {
        command_id: u64,
        authorization_id: String,
        decision: ToolAuthorizationDecision,
    },
    RecoveryChanged {
        command_id: u64,
        action: DesktopRecoveryAction,
        recovery_id: String,
        recovery: DesktopRuntimeRecoverySnapshot,
    },
    FileReviewed {
        command_id: u64,
        review: CodingAgentFileReview,
    },
    ExternalEditorTargetValidated {
        command_id: u64,
        target: CodingAgentExternalEditorTarget,
    },
    MergeProposalsListed {
        command_id: u64,
        proposals: Vec<CodingAgentMergeProposal>,
    },
    ChildWorktreeMerged {
        command_id: u64,
        worktree_id: String,
        applied: usize,
    },
    ChildWorktreeDiscarded {
        command_id: u64,
        worktree_id: String,
    },
    PromptFinished {
        command_id: u64,
        operation_id: String,
        snapshot: DesktopRuntimeHydratedSnapshot,
        error: Option<DesktopRuntimeError>,
    },
    CommandRejected {
        command_id: u64,
        command: DesktopRuntimeCommandKind,
        code: String,
        message: String,
    },
    RuntimeFailed {
        error: DesktopRuntimeError,
    },
    Stopped,
}

impl DesktopRuntimeUpdate {
    #[cfg(test)]
    pub(crate) fn product_event(event: CodingAgentProductEvent) -> Self {
        let session_id = event
            .session_id()
            .unwrap_or_else(|| event.stream_id())
            .to_owned();
        Self::ProductEvent { session_id, event }
    }

    pub(crate) const fn kind_label(&self) -> &'static str {
        match self {
            Self::Reloaded { .. } => "reloaded",
            Self::Resynced { .. } => "resynced",
            Self::SessionChanged { .. } => "session_changed",
            Self::SessionClosed { .. } => "session_closed",
            Self::SessionDeleted { .. } => "session_deleted",
            Self::SessionsListed { .. } => "sessions_listed",
            Self::SessionRenamed { .. } => "session_renamed",
            Self::SessionNameObserved { .. } => "session_name_observed",
            Self::SelectionChanged { .. } => "selection_changed",
            Self::PromptAccepted { .. } => "prompt_accepted",
            Self::PromptAcceptedWithSession { .. } => "prompt_accepted_with_session",
            Self::PromptRejectedWithSession { .. } => "prompt_rejected_with_session",
            Self::PromptStarted { .. } => "prompt_started",
            Self::ProductEvent { .. } => "product_event",
            Self::ResyncRequired { .. } => "resync_required",
            Self::ControlAccepted { .. } => "control_accepted",
            Self::AuthorizationDecisionAccepted { .. } => "authorization_decision_accepted",
            Self::RecoveryChanged { .. } => "recovery_changed",
            Self::FileReviewed { .. } => "file_reviewed",
            Self::ExternalEditorTargetValidated { .. } => "external_editor_target_validated",
            Self::MergeProposalsListed { .. } => "merge_proposals_listed",
            Self::ChildWorktreeMerged { .. } => "child_worktree_merged",
            Self::ChildWorktreeDiscarded { .. } => "child_worktree_discarded",
            Self::PromptFinished { .. } => "prompt_finished",
            Self::CommandRejected { .. } => "command_rejected",
            Self::RuntimeFailed { .. } => "runtime_failed",
            Self::Stopped => "stopped",
        }
    }
}

pub(super) fn validate_session_id(session_id: &str) -> Result<(), DesktopCommandAdmissionError> {
    if session_id.is_empty() {
        return Err(DesktopCommandAdmissionError::InvalidSessionId {
            message: "session id must not be empty".into(),
        });
    }
    if session_id.len() > MAX_SESSION_ID_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidSessionId {
            message: format!("session id exceeds {MAX_SESSION_ID_BYTES} bytes"),
        });
    }
    Ok(())
}

pub(super) fn validate_session_name(
    name: Option<&str>,
) -> Result<(), DesktopCommandAdmissionError> {
    if name.is_some_and(|name| name.len() > MAX_SESSION_NAME_BYTES || name.contains('\0')) {
        return Err(DesktopCommandAdmissionError::InvalidSessionName {
            message: format!(
                "session name must not contain NUL or exceed {MAX_SESSION_NAME_BYTES} bytes"
            ),
        });
    }
    Ok(())
}

pub(super) fn validate_prompt_target(
    target: &DesktopPromptTarget,
) -> Result<(), DesktopCommandAdmissionError> {
    match target {
        DesktopPromptTarget::Existing { session_id } => validate_session_id(session_id),
        DesktopPromptTarget::New {
            workspace,
            model_id,
            profile_id,
        } => {
            validate_selection_id("model", model_id)?;
            validate_selection_id("profile", profile_id)?;
            if let CodingAgentWorkspaceSelection::Project { cwd } = workspace {
                let display = cwd.to_string_lossy();
                if display.len() > MAX_WORKSPACE_PATH_BYTES {
                    return Err(DesktopCommandAdmissionError::InvalidPromptTarget {
                        message: format!("project path exceeds {MAX_WORKSPACE_PATH_BYTES} bytes"),
                    });
                }
                if display.contains('\0') {
                    return Err(DesktopCommandAdmissionError::InvalidPromptTarget {
                        message: "project path contains a NUL byte".into(),
                    });
                }
            }
            workspace
                .clone()
                .resolve(global_config_directory())
                .map(|_| ())
                .map_err(|error| DesktopCommandAdmissionError::InvalidPromptTarget {
                    message: bounded_utf8_prefix(&error.to_string(), 1024),
                })
        }
    }
}

pub(super) fn validate_runtime_owner_target(
    target: &DesktopRuntimeOwnerTarget,
) -> Result<(), DesktopCommandAdmissionError> {
    match target {
        DesktopRuntimeOwnerTarget::Home => Ok(()),
        DesktopRuntimeOwnerTarget::Session { session_id } => validate_session_id(session_id),
    }
}

pub(super) fn bounded_utf8_prefix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    value[..end].to_owned()
}

pub(super) fn validate_prompt_with_attachments(
    prompt: &str,
    attachments: &[PathBuf],
) -> Result<(), DesktopCommandAdmissionError> {
    if prompt.trim().is_empty() && attachments.is_empty() {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: "prompt or attachment must not be empty".into(),
        });
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: format!("prompt exceeds {MAX_PROMPT_BYTES} bytes"),
        });
    }
    validate_prompt_attachments(attachments)?;
    Ok(())
}

pub fn validate_prompt_attachments(
    attachments: &[PathBuf],
) -> Result<(), DesktopCommandAdmissionError> {
    if attachments.len() > MAX_PROMPT_ATTACHMENTS {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: format!("prompt has more than {MAX_PROMPT_ATTACHMENTS} attachments"),
        });
    }
    let mut total_path_bytes = 0usize;
    for attachment in attachments {
        validate_prompt_attachment_path(attachment)?;
        total_path_bytes = total_path_bytes.saturating_add(
            attachment
                .to_str()
                .map(str::len)
                .expect("attachment path was validated as UTF-8"),
        );
        if total_path_bytes > MAX_PROMPT_ATTACHMENT_PATH_TOTAL_BYTES {
            return Err(DesktopCommandAdmissionError::InvalidPrompt {
                message: format!(
                    "attachment paths exceed {MAX_PROMPT_ATTACHMENT_PATH_TOTAL_BYTES} bytes"
                ),
            });
        }
    }
    Ok(())
}

fn validate_prompt_attachment_path(path: &Path) -> Result<(), DesktopCommandAdmissionError> {
    let Some(path) = path.to_str() else {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: "attachment path must be valid UTF-8".into(),
        });
    };
    if path.is_empty() || path.contains('\0') {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: "attachment path must not be empty or contain NUL".into(),
        });
    }
    if path.len() > MAX_PROMPT_ATTACHMENT_PATH_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: format!("attachment path exceeds {MAX_PROMPT_ATTACHMENT_PATH_BYTES} bytes"),
        });
    }
    Ok(())
}

pub(super) fn validate_control_text(text: &str) -> Result<(), DesktopCommandAdmissionError> {
    if text.trim().is_empty() {
        return Err(DesktopCommandAdmissionError::InvalidControlText {
            message: "control text must not be empty".into(),
        });
    }
    if text.len() > MAX_CONTROL_TEXT_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidControlText {
            message: format!("control text exceeds {MAX_CONTROL_TEXT_BYTES} bytes"),
        });
    }
    Ok(())
}

pub(super) fn validate_file_review_request(
    request: &CodingAgentFileReviewRequest,
) -> Result<(), DesktopCommandAdmissionError> {
    for (field, value) in [
        ("operation", request.change.operation_id.as_str()),
        ("path", request.change.path.as_str()),
    ] {
        if value.is_empty() {
            return Err(DesktopCommandAdmissionError::InvalidFileReview {
                message: format!("{field} must not be empty"),
            });
        }
        let limit = if field == "path" {
            MAX_FILE_REVIEW_PATH_BYTES
        } else {
            MAX_FILE_REVIEW_ID_BYTES
        };
        if value.len() > limit {
            return Err(DesktopCommandAdmissionError::InvalidFileReview {
                message: format!("{field} exceeds {limit} bytes"),
            });
        }
    }
    if request
        .change
        .tool_call_id
        .as_ref()
        .is_some_and(|tool_call_id| {
            tool_call_id.is_empty() || tool_call_id.len() > MAX_FILE_REVIEW_ID_BYTES
        })
    {
        return Err(DesktopCommandAdmissionError::InvalidFileReview {
            message: "tool-call id is empty or oversized".into(),
        });
    }
    Ok(())
}

pub(super) fn validate_authorization_identity(
    identity: &ToolAuthorizationIdentity,
) -> Result<(), DesktopCommandAdmissionError> {
    for (field, value) in [
        ("authorization", identity.authorization_id.as_str()),
        ("operation", identity.operation_id.as_str()),
        ("turn", identity.turn_id.as_str()),
        ("tool call", identity.tool_call_id.as_str()),
    ] {
        if value.is_empty() {
            return Err(DesktopCommandAdmissionError::InvalidAuthorizationId {
                message: format!("{field} id must not be empty"),
            });
        }
        if value.len() > MAX_AUTHORIZATION_ID_BYTES {
            return Err(DesktopCommandAdmissionError::InvalidAuthorizationId {
                message: format!("{field} id exceeds {MAX_AUTHORIZATION_ID_BYTES} bytes"),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_recovery_identity(
    identity: &DesktopRecoveryIdentity,
) -> Result<(), DesktopCommandAdmissionError> {
    for (field, value) in [
        ("operation", identity.operation_id.as_str()),
        ("recovery", identity.recovery_id.as_str()),
    ] {
        if value.is_empty() {
            return Err(DesktopCommandAdmissionError::InvalidRecoveryId {
                message: format!("{field} id must not be empty"),
            });
        }
        if value.len() > MAX_RECOVERY_ID_BYTES {
            return Err(DesktopCommandAdmissionError::InvalidRecoveryId {
                message: format!("{field} id exceeds {MAX_RECOVERY_ID_BYTES} bytes"),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_selection_id(
    selection: &str,
    id: &str,
) -> Result<(), DesktopCommandAdmissionError> {
    if id.is_empty() {
        return Err(DesktopCommandAdmissionError::InvalidSelectionId {
            message: format!("{selection} id must not be empty"),
        });
    }
    if id.len() > MAX_SELECTION_ID_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidSelectionId {
            message: format!("{selection} id exceeds {MAX_SELECTION_ID_BYTES} bytes"),
        });
    }
    Ok(())
}
