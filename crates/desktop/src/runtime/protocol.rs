use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::client::{
    CodingAgentControlReceipt, CodingAgentRecoveryPending, CodingAgentSnapshot,
};
use coding_agent::api::embedding::{CodingAgentEmbeddingSnapshot, CodingAgentThinkingLevel};
use coding_agent::api::embedding::{CodingAgentWorkspaceSelection, global_config_directory};
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::event::{CodingAgentProductEvent, CodingAgentRecoveryResolution};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use coding_agent::api::view::{
    CodingAgentTranscriptSnapshot, CodingAgentWorkspaceKind, CodingAgentWorkspaceMigration,
    CodingAgentWorkspaceMigrationOutcome, CodingAgentWorkspaceOverview,
};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::file_review::{DesktopExternalEditorConfig, DesktopExternalEditorLaunchError};

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

pub(super) enum DesktopRuntimeCommand {
    Reload {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
    },
    Resync {
        command_id: u64,
        session_id: Option<String>,
    },
    CreateSession {
        command_id: u64,
    },
    OpenSession {
        command_id: u64,
        session_id: String,
    },
    CloseSession {
        command_id: u64,
        session_id: String,
    },
    ListSessions {
        command_id: u64,
    },
    RenameSession {
        command_id: u64,
        session_id: String,
        name: Option<String>,
    },
    SelectModel {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        model_id: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    },
    SelectSessionProfile {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        profile_id: String,
    },
    SubmitPrompt {
        command_id: u64,
        target: DesktopPromptTarget,
        prompt: String,
        attachments: Vec<PathBuf>,
        thinking_level: Option<CodingAgentThinkingLevel>,
    },
    Abort {
        command_id: u64,
        session_id: Option<String>,
    },
    Steer {
        command_id: u64,
        session_id: Option<String>,
        text: String,
    },
    FollowUp {
        command_id: u64,
        session_id: Option<String>,
        text: String,
    },
    DecideToolAuthorization {
        command_id: u64,
        session_id: Option<String>,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
    RetryRecovery {
        command_id: u64,
        session_id: Option<String>,
        identity: DesktopRecoveryIdentity,
    },
    ResolveRecovery {
        command_id: u64,
        session_id: Option<String>,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    },
    ReviewChangedFile {
        command_id: u64,
        session_id: String,
        request: CodingAgentFileReviewRequest,
    },
    OpenExternalEditor {
        command_id: u64,
        session_id: String,
        target: CodingAgentExternalEditorTarget,
        editor: DesktopExternalEditorConfig,
    },
}

/// Explicit owner for context-level desktop commands.
#[derive(Clone, PartialEq, Eq)]
pub enum DesktopRuntimeOwnerTarget {
    Home,
    Session { session_id: String },
}

impl DesktopRuntimeOwnerTarget {
    pub const fn home() -> Self {
        Self::Home
    }

    pub fn session(session_id: impl Into<String>) -> Self {
        Self::Session {
            session_id: session_id.into(),
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Home => None,
            Self::Session { session_id } => Some(session_id),
        }
    }
}

impl fmt::Debug for DesktopRuntimeOwnerTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopRuntimeOwnerTarget")
            .field(
                "kind",
                &match self {
                    Self::Home => "home",
                    Self::Session { .. } => "session",
                },
            )
            .finish_non_exhaustive()
    }
}

/// Explicit target for a desktop prompt submission.
///
/// New sessions must carry every Home selection needed to construct their
/// future per-workspace runtime owner. Existing sessions carry only durable
/// identity, so a cwd or workspace override cannot be represented.
#[derive(Clone, PartialEq, Eq)]
pub enum DesktopPromptTarget {
    New {
        workspace: CodingAgentWorkspaceSelection,
        model_id: String,
        profile_id: String,
    },
    Existing {
        session_id: String,
    },
}

impl DesktopPromptTarget {
    pub fn new(
        workspace: CodingAgentWorkspaceSelection,
        model_id: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Self {
        Self::New {
            workspace,
            model_id: model_id.into(),
            profile_id: profile_id.into(),
        }
    }

    pub fn existing(session_id: impl Into<String>) -> Self {
        Self::Existing {
            session_id: session_id.into(),
        }
    }

    pub fn existing_session_id(&self) -> Option<&str> {
        match self {
            Self::New { .. } => None,
            Self::Existing { session_id } => Some(session_id),
        }
    }

    const fn kind_label(&self) -> &'static str {
        match self {
            Self::New { .. } => "new",
            Self::Existing { .. } => "existing",
        }
    }
}

impl fmt::Debug for DesktopPromptTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopPromptTarget")
            .field("kind", &self.kind_label())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DesktopRuntimeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("DesktopRuntimeCommand");
        debug
            .field("kind", &self.kind())
            .field("command_id", &self.command_id());
        if let Self::SubmitPrompt { target, .. } = self {
            debug.field("prompt_target", &target.kind_label());
        }
        debug.finish_non_exhaustive()
    }
}

impl DesktopRuntimeCommand {
    pub(super) const fn command_id(&self) -> u64 {
        match self {
            Self::Reload { command_id, .. }
            | Self::Resync { command_id, .. }
            | Self::CreateSession { command_id }
            | Self::OpenSession { command_id, .. }
            | Self::CloseSession { command_id, .. }
            | Self::ListSessions { command_id }
            | Self::RenameSession { command_id, .. }
            | Self::SelectModel { command_id, .. }
            | Self::SelectSessionProfile { command_id, .. }
            | Self::SubmitPrompt { command_id, .. }
            | Self::Abort { command_id, .. }
            | Self::Steer { command_id, .. }
            | Self::FollowUp { command_id, .. }
            | Self::DecideToolAuthorization { command_id, .. }
            | Self::RetryRecovery { command_id, .. }
            | Self::ResolveRecovery { command_id, .. }
            | Self::ReviewChangedFile { command_id, .. }
            | Self::OpenExternalEditor { command_id, .. } => *command_id,
        }
    }

    pub(super) const fn kind(&self) -> DesktopRuntimeCommandKind {
        match self {
            Self::Reload { .. } => DesktopRuntimeCommandKind::Reload,
            Self::Resync { .. } => DesktopRuntimeCommandKind::Resync,
            Self::CreateSession { .. } => DesktopRuntimeCommandKind::CreateSession,
            Self::OpenSession { .. } => DesktopRuntimeCommandKind::OpenSession,
            Self::CloseSession { .. } => DesktopRuntimeCommandKind::CloseSession,
            Self::ListSessions { .. } => DesktopRuntimeCommandKind::ListSessions,
            Self::RenameSession { .. } => DesktopRuntimeCommandKind::RenameSession,
            Self::SelectModel { .. } => DesktopRuntimeCommandKind::SelectModel,
            Self::SelectSessionProfile { .. } => DesktopRuntimeCommandKind::SelectSessionProfile,
            Self::SubmitPrompt { .. } => DesktopRuntimeCommandKind::SubmitPrompt,
            Self::Abort { .. } => DesktopRuntimeCommandKind::Abort,
            Self::Steer { .. } => DesktopRuntimeCommandKind::Steer,
            Self::FollowUp { .. } => DesktopRuntimeCommandKind::FollowUp,
            Self::DecideToolAuthorization { .. } => {
                DesktopRuntimeCommandKind::DecideToolAuthorization
            }
            Self::RetryRecovery { .. } => DesktopRuntimeCommandKind::RetryRecovery,
            Self::ResolveRecovery { .. } => DesktopRuntimeCommandKind::ResolveRecovery,
            Self::ReviewChangedFile { .. } => DesktopRuntimeCommandKind::ReviewChangedFile,
            Self::OpenExternalEditor { .. } => DesktopRuntimeCommandKind::OpenExternalEditor,
        }
    }

    pub(super) fn target_session_id(&self) -> Option<&str> {
        match self {
            Self::OpenSession { session_id, .. }
            | Self::CloseSession { session_id, .. }
            | Self::RenameSession { session_id, .. } => Some(session_id),
            Self::Reload { target, .. }
            | Self::SelectModel { target, .. }
            | Self::SelectSessionProfile { target, .. } => target.session_id(),
            Self::SubmitPrompt { target, .. } => target.existing_session_id(),
            Self::ReviewChangedFile { session_id, .. }
            | Self::OpenExternalEditor { session_id, .. } => Some(session_id),
            Self::Resync { session_id, .. }
            | Self::Abort { session_id, .. }
            | Self::Steer { session_id, .. }
            | Self::FollowUp { session_id, .. }
            | Self::DecideToolAuthorization { session_id, .. }
            | Self::RetryRecovery { session_id, .. }
            | Self::ResolveRecovery { session_id, .. } => session_id.as_deref(),
            Self::CreateSession { .. } | Self::ListSessions { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeCommandKind {
    Reload,
    Resync,
    CreateSession,
    OpenSession,
    CloseSession,
    ListSessions,
    RenameSession,
    SelectModel,
    SelectSessionProfile,
    SubmitPrompt,
    Abort,
    Steer,
    FollowUp,
    DecideToolAuthorization,
    RetryRecovery,
    ResolveRecovery,
    ReviewChangedFile,
    OpenExternalEditor,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeReadySnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeHydratedSnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: CodingAgentSnapshot,
    pub transcript: CodingAgentTranscriptSnapshot,
    pub pending_recoveries: Vec<CodingAgentRecoveryPending>,
}

/// Narrow project/session replacement for metadata-only desktop commands.
///
/// This type intentionally cannot carry a transcript or durable recovery
/// payload. Reload and selection commands therefore cannot accidentally
/// hydrate or clone the conversation while refreshing product metadata.
#[derive(Debug, Clone)]
pub struct DesktopRuntimeMetadataSnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: Option<CodingAgentSnapshot>,
}

/// Narrow recovery replacement without durable transcript content.
#[derive(Debug, Clone)]
pub struct DesktopRuntimeRecoverySnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: CodingAgentSnapshot,
    pub pending_recoveries: Vec<CodingAgentRecoveryPending>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSessionCatalogEntry {
    pub session_id: String,
    pub name: Option<String>,
    pub workspace: CodingAgentWorkspaceOverview,
    pub workspace_migration: CodingAgentWorkspaceMigration,
    pub created_at: String,
    pub updated_at: String,
    pub active_leaf_id: Option<String>,
}

impl Default for DesktopSessionCatalogEntry {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            name: None,
            workspace: CodingAgentWorkspaceOverview {
                group_id: "legacy:unscoped".into(),
                kind: CodingAgentWorkspaceKind::Legacy,
                display_name: "Legacy session".into(),
                display_path: None,
            },
            workspace_migration: CodingAgentWorkspaceMigration {
                outcome: CodingAgentWorkspaceMigrationOutcome::Unavailable,
                diagnostic: None,
            },
            created_at: String::new(),
            updated_at: String::new(),
            active_leaf_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DesktopRuntimeResyncSnapshot {
    Metadata(DesktopRuntimeMetadataSnapshot),
    Hydrated(DesktopRuntimeHydratedSnapshot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeSelectionKind {
    Model,
    SessionProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRecoveryIdentity {
    pub operation_id: String,
    pub recovery_id: String,
    pub record_version: u64,
    pub descriptor_revision: u16,
    pub capability_generation: Option<u64>,
    pub attempt_count: u32,
}

impl From<&CodingAgentRecoveryPending> for DesktopRecoveryIdentity {
    fn from(pending: &CodingAgentRecoveryPending) -> Self {
        Self {
            operation_id: pending.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
            attempt_count: pending.attempt_count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRecoveryAction {
    Retry,
    MarkFailed,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum DesktopBridgeError {
    #[error("invalid desktop input: {message}")]
    Input { message: String },
    #[error("desktop session error: {message}")]
    Session { message: String },
    #[error("desktop runtime is busy: {operation}")]
    Busy { operation: String },
    #[error("desktop session target is ambiguous: {message}")]
    SessionTarget { message: String },
    #[error("desktop session limit of {limit} has been reached")]
    SessionLimit { limit: usize },
    #[error("desktop session workspace is unavailable: {message}")]
    WorkspaceUnavailable { message: String },
    #[error("{0}")]
    Product(CodingAgentPublicError),
    #[error("external editor launch failed")]
    ExternalEditor,
}

impl From<CodingAgentPublicError> for DesktopBridgeError {
    fn from(error: CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}

impl From<DesktopExternalEditorLaunchError> for DesktopBridgeError {
    fn from(_: DesktopExternalEditorLaunchError) -> Self {
        Self::ExternalEditor
    }
}

#[cfg(test)]
impl DesktopBridgeError {
    pub(super) fn cancelled_for_tests() -> Self {
        Self::Product(CodingAgentPublicError {
            category: coding_agent::api::error::CodingAgentErrorCategory::Cancellation,
            code: "cancelled".into(),
            retryable: true,
            summary: "The operation was cancelled.".into(),
            context: coding_agent::api::error::CodingAgentErrorContext::None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRuntimeError {
    pub code: String,
    pub message: String,
}

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
    ExternalEditorOpened {
        command_id: u64,
        project_relative_path: String,
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
            Self::ExternalEditorOpened { .. } => "external_editor_opened",
            Self::PromptFinished { .. } => "prompt_finished",
            Self::CommandRejected { .. } => "command_rejected",
            Self::RuntimeFailed { .. } => "runtime_failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopRuntimeStartError {
    #[error("failed to spawn desktop runtime thread")]
    Spawn(#[source] std::io::Error),
    #[error("desktop runtime initialization failed ({code}): {message}")]
    Initialization { code: String, message: String },
    #[error("desktop runtime thread closed during initialization")]
    InitializationChannelClosed,
    #[error("desktop runtime thread panicked during initialization")]
    InitializationThreadPanicked,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DesktopCommandAdmissionError {
    #[error("desktop runtime command queue is full")]
    QueueFull,
    #[error("desktop runtime command queue is closed")]
    RuntimeClosed,
    #[error("invalid session id: {message}")]
    InvalidSessionId { message: String },
    #[error("invalid session name: {message}")]
    InvalidSessionName { message: String },
    #[error("invalid prompt target: {message}")]
    InvalidPromptTarget { message: String },
    #[error("invalid prompt: {message}")]
    InvalidPrompt { message: String },
    #[error("invalid control text: {message}")]
    InvalidControlText { message: String },
    #[error("invalid authorization id: {message}")]
    InvalidAuthorizationId { message: String },
    #[error("invalid recovery id: {message}")]
    InvalidRecoveryId { message: String },
    #[error("invalid selection id: {message}")]
    InvalidSelectionId { message: String },
    #[error("invalid changed-file review request: {message}")]
    InvalidFileReview { message: String },
    #[error("invalid external editor configuration: {message}")]
    InvalidExternalEditor { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopRuntimeShutdownError {
    #[error("desktop runtime thread panicked")]
    RuntimePanicked,
}

/// Dedicated owner of the desktop product context and active coding session.
///
/// All configuration/session I/O occurs on the runtime thread. GPUI callers
/// communicate exclusively through bounded queues. Priority updates carry
/// terminal/control/diagnostic state; high-frequency message data uses a
/// separate bounded lane and converts overflow into a typed resync request.
pub(super) trait DesktopRuntimeErrorSource {
    fn project_runtime_error(&self) -> DesktopRuntimeError;
}

impl DesktopRuntimeErrorSource for CodingAgentPublicError {
    fn project_runtime_error(&self) -> DesktopRuntimeError {
        DesktopRuntimeError {
            code: self.code.clone(),
            message: self.summary.clone(),
        }
    }
}

impl DesktopRuntimeErrorSource for DesktopBridgeError {
    fn project_runtime_error(&self) -> DesktopRuntimeError {
        match self {
            DesktopBridgeError::Product(public) => public.project_runtime_error(),
            DesktopBridgeError::Input { .. } => {
                local_runtime_error("input", "The desktop request is invalid.")
            }
            DesktopBridgeError::Session { message } => local_runtime_error("session", message),
            DesktopBridgeError::Busy { .. } => {
                local_runtime_error("busy", "The desktop runtime is busy.")
            }
            DesktopBridgeError::SessionTarget { message } => {
                local_runtime_error("session_target", message)
            }
            DesktopBridgeError::SessionLimit { limit } => local_runtime_error(
                "session_limit_reached",
                &format!("At most {limit} desktop sessions can be open at once."),
            ),
            DesktopBridgeError::WorkspaceUnavailable { message } => {
                local_runtime_error("workspace_unavailable", message)
            }
            DesktopBridgeError::ExternalEditor => local_runtime_error(
                "external_editor_unavailable",
                "The configured external editor could not be started.",
            ),
        }
    }
}

pub(super) fn runtime_error(error: &impl DesktopRuntimeErrorSource) -> DesktopRuntimeError {
    error.project_runtime_error()
}

pub(super) fn local_runtime_error(code: &str, message: &str) -> DesktopRuntimeError {
    DesktopRuntimeError {
        code: code.to_owned(),
        message: message.to_owned(),
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

#[allow(
    dead_code,
    reason = "compatibility entry point for text-only desktop clients"
)]
pub(super) fn validate_prompt(prompt: &str) -> Result<(), DesktopCommandAdmissionError> {
    validate_prompt_with_attachments(prompt, &[])
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
