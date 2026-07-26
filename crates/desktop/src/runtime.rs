use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentControlId,
    CodingAgentControlReceipt, CodingAgentDraftId, CodingAgentFreshSnapshotRecovery,
    CodingAgentReconnect, CodingAgentReconnectDelivery, CodingAgentReconnectReceiver,
    CodingAgentRecoveryPending, CodingAgentRecoveryReason, CodingAgentSnapshot,
    CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentEmbeddingSnapshot,
    CodingAgentThinkingLevel,
};
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentProductEventFamily,
    CodingAgentRecoveryResolution,
};
use coding_agent::api::operation::{
    CodingAgentOperation, CodingAgentOperationOutcome, PromptInvocation,
};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use coding_agent::api::runtime::{
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest, CodingAgentSession,
};
use coding_agent::api::view::{CodingAgentTranscriptSnapshot, ProfileId};
use tokio::runtime;
use tokio::sync::{mpsc, watch};
use tokio::task;

use crate::file_review::{
    DesktopExternalEditorConfig, DesktopExternalEditorLaunchError, launch_external_editor,
};

pub const DESKTOP_COMMAND_QUEUE_CAPACITY: usize = 64;
pub const DESKTOP_UPDATE_QUEUE_CAPACITY: usize = 128;
pub const DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY: usize = 64;
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;
pub const MAX_CONTROL_TEXT_BYTES: usize = 64 * 1024;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_AUTHORIZATION_ID_BYTES: usize = 256;
const MAX_SELECTION_ID_BYTES: usize = 256;
const MAX_RECOVERY_ID_BYTES: usize = 1024;
const MAX_FILE_REVIEW_ID_BYTES: usize = 1024;
const MAX_FILE_REVIEW_PATH_BYTES: usize = 16 * 1024;
pub const MAX_DESKTOP_SESSION_CATALOG: usize = 128;
const RUNTIME_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
const STREAMING_DELIVERY_COALESCE_WINDOW: Duration = Duration::from_millis(16);
const MAX_STREAMING_DELIVERIES_PER_BATCH: usize = 64;
const DESKTOP_CLIENT_ID: &str = "evo-desktop";

fn build_desktop_runtime() -> std::io::Result<runtime::Runtime> {
    runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
}

#[derive(Debug)]
enum DesktopRuntimeCommand {
    Reload {
        command_id: u64,
    },
    Resync {
        command_id: u64,
    },
    CreateSession {
        command_id: u64,
    },
    OpenSession {
        command_id: u64,
        session_id: String,
    },
    ListSessions {
        command_id: u64,
    },
    SelectModel {
        command_id: u64,
        model_id: String,
    },
    SelectSessionProfile {
        command_id: u64,
        profile_id: String,
    },
    SubmitPrompt {
        command_id: u64,
        prompt: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    },
    Abort {
        command_id: u64,
    },
    Steer {
        command_id: u64,
        text: String,
    },
    FollowUp {
        command_id: u64,
        text: String,
    },
    DecideToolAuthorization {
        command_id: u64,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
    RetryRecovery {
        command_id: u64,
        identity: DesktopRecoveryIdentity,
    },
    ResolveRecovery {
        command_id: u64,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    },
    ReviewChangedFile {
        command_id: u64,
        request: CodingAgentFileReviewRequest,
    },
    OpenExternalEditor {
        command_id: u64,
        target: CodingAgentExternalEditorTarget,
        editor: DesktopExternalEditorConfig,
    },
}

impl DesktopRuntimeCommand {
    const fn command_id(&self) -> u64 {
        match self {
            Self::Reload { command_id }
            | Self::Resync { command_id }
            | Self::CreateSession { command_id }
            | Self::OpenSession { command_id, .. }
            | Self::ListSessions { command_id }
            | Self::SelectModel { command_id, .. }
            | Self::SelectSessionProfile { command_id, .. }
            | Self::SubmitPrompt { command_id, .. }
            | Self::Abort { command_id }
            | Self::Steer { command_id, .. }
            | Self::FollowUp { command_id, .. }
            | Self::DecideToolAuthorization { command_id, .. }
            | Self::RetryRecovery { command_id, .. }
            | Self::ResolveRecovery { command_id, .. }
            | Self::ReviewChangedFile { command_id, .. }
            | Self::OpenExternalEditor { command_id, .. } => *command_id,
        }
    }

    const fn kind(&self) -> DesktopRuntimeCommandKind {
        match self {
            Self::Reload { .. } => DesktopRuntimeCommandKind::Reload,
            Self::Resync { .. } => DesktopRuntimeCommandKind::Resync,
            Self::CreateSession { .. } => DesktopRuntimeCommandKind::CreateSession,
            Self::OpenSession { .. } => DesktopRuntimeCommandKind::OpenSession,
            Self::ListSessions { .. } => DesktopRuntimeCommandKind::ListSessions,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeCommandKind {
    Reload,
    Resync,
    CreateSession,
    OpenSession,
    ListSessions,
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
    pub session: CodingAgentSnapshot,
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
    pub updated_at: String,
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
enum DesktopBridgeError {
    #[error("invalid desktop input: {message}")]
    Input { message: String },
    #[error("desktop session error: {message}")]
    Session { message: String },
    #[error("desktop runtime is busy: {operation}")]
    Busy { operation: String },
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
    fn cancelled_for_tests() -> Self {
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
    SessionsListed {
        command_id: u64,
        sessions: Vec<DesktopSessionCatalogEntry>,
        omitted: usize,
    },
    SelectionChanged {
        command_id: u64,
        selection: DesktopRuntimeSelectionKind,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    PromptAccepted {
        command_id: u64,
    },
    PromptStarted {
        command_id: u64,
        operation_id: String,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    ProductEvent {
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
pub struct DesktopRuntimeBridge {
    shutdown: DesktopRuntimeShutdownGuard,
    commands: Option<mpsc::Sender<DesktopRuntimeCommand>>,
    events: DesktopRuntimeEventStream,
}

/// Sole ordered delivery side of the desktop runtime.
pub struct DesktopRuntimeEventStream {
    priority_updates: mpsc::Receiver<DesktopRuntimeUpdate>,
    data_updates: mpsc::Receiver<DesktopRuntimeUpdate>,
    pending_priority_update: Option<DesktopRuntimeUpdate>,
    pending_data_update: Option<DesktopRuntimeUpdate>,
}

/// Sole shutdown signal and runtime-thread join owner.
pub struct DesktopRuntimeShutdownGuard {
    shutdown: watch::Sender<bool>,
    runtime_thread: Option<JoinHandle<()>>,
}

/// Cloneable, non-blocking command side of the desktop runtime bridge.
#[derive(Clone)]
pub struct DesktopRuntimeCommandHandle {
    commands: mpsc::Sender<DesktopRuntimeCommand>,
}

/// Non-blocking startup handle for a desktop runtime.
///
/// GPUI owns this value while project configuration and the initial session
/// load on the dedicated runtime thread. [`Self::try_ready`] never waits.
pub struct DesktopRuntimeBootstrap {
    ready: std_mpsc::Receiver<Result<DesktopRuntimeHydratedSnapshot, DesktopRuntimeError>>,
    bridge: Option<DesktopRuntimeBridge>,
}

impl DesktopRuntimeBootstrap {
    pub fn try_ready(
        &mut self,
    ) -> Result<
        Option<(DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot)>,
        DesktopRuntimeStartError,
    > {
        match self.ready.try_recv() {
            Ok(result) => self.finish(result).map(Some),
            Err(std_mpsc::TryRecvError::Empty) => Ok(None),
            Err(std_mpsc::TryRecvError::Disconnected) => self.finish_disconnected_initialization(),
        }
    }

    /// Blocking startup is reserved for tests and explicitly non-GPUI callers.
    #[cfg(test)]
    pub fn wait_blocking(
        mut self,
    ) -> Result<(DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot), DesktopRuntimeStartError>
    {
        match self.ready.recv() {
            Ok(result) => self.finish(result),
            Err(_) => self.finish_disconnected_initialization().and_then(|ready| {
                ready.ok_or(DesktopRuntimeStartError::InitializationChannelClosed)
            }),
        }
    }

    fn finish(
        &mut self,
        result: Result<DesktopRuntimeHydratedSnapshot, DesktopRuntimeError>,
    ) -> Result<(DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot), DesktopRuntimeStartError>
    {
        let mut bridge = self
            .bridge
            .take()
            .ok_or(DesktopRuntimeStartError::InitializationChannelClosed)?;
        match result {
            Ok(snapshot) => Ok((bridge, snapshot)),
            Err(error) => {
                let join = bridge.join_runtime_thread();
                if matches!(join, Err(DesktopRuntimeShutdownError::RuntimePanicked)) {
                    return Err(DesktopRuntimeStartError::InitializationThreadPanicked);
                }
                Err(DesktopRuntimeStartError::Initialization {
                    code: error.code,
                    message: error.message,
                })
            }
        }
    }

    fn finish_disconnected_initialization(
        &mut self,
    ) -> Result<
        Option<(DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot)>,
        DesktopRuntimeStartError,
    > {
        let Some(mut bridge) = self.bridge.take() else {
            return Err(DesktopRuntimeStartError::InitializationChannelClosed);
        };
        match bridge.join_runtime_thread() {
            Ok(()) => Err(DesktopRuntimeStartError::InitializationChannelClosed),
            Err(DesktopRuntimeShutdownError::RuntimePanicked) => {
                Err(DesktopRuntimeStartError::InitializationThreadPanicked)
            }
        }
    }
}

impl Drop for DesktopRuntimeBootstrap {
    fn drop(&mut self) {
        drop(self.bridge.take());
    }
}

impl DesktopRuntimeBridge {
    #[cfg(test)]
    pub(crate) fn disconnected_for_test() -> Self {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        drop(command_rx);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        drop(priority_update_tx);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        drop(data_update_tx);
        let (shutdown, _) = watch::channel(false);
        Self {
            shutdown: DesktopRuntimeShutdownGuard {
                shutdown,
                runtime_thread: None,
            },
            commands: Some(commands),
            events: DesktopRuntimeEventStream {
                priority_updates,
                data_updates,
                pending_priority_update: None,
                pending_data_update: None,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn instrumented_for_test() -> (Self, DesktopRuntimeTestHarness) {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        (
            Self {
                shutdown: DesktopRuntimeShutdownGuard {
                    shutdown,
                    runtime_thread: None,
                },
                commands: Some(commands),
                events: DesktopRuntimeEventStream {
                    priority_updates,
                    data_updates,
                    pending_priority_update: None,
                    pending_data_update: None,
                },
            },
            DesktopRuntimeTestHarness {
                commands: command_rx,
                _priority_update_tx: priority_update_tx,
                _data_update_tx: data_update_tx,
            },
        )
    }

    /// Spawn runtime initialization without waiting for configuration/session I/O.
    pub fn spawn(
        options: CodingAgentEmbeddingOptions,
    ) -> Result<DesktopRuntimeBootstrap, DesktopRuntimeStartError> {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);

        let runtime_thread = thread::Builder::new()
            .name("desktop-runtime".into())
            .spawn(move || {
                let runtime = match build_desktop_runtime() {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready_tx.send(Err(local_runtime_error(
                            "runtime_initialization",
                            "The desktop runtime could not be initialized.",
                        )));
                        return;
                    }
                };
                runtime.block_on(run_runtime(
                    options,
                    command_rx,
                    shutdown_rx,
                    priority_update_tx,
                    data_update_tx,
                    ready_tx,
                ));
            })
            .map_err(DesktopRuntimeStartError::Spawn)?;

        Ok(DesktopRuntimeBootstrap {
            ready: ready_rx,
            bridge: Some(Self {
                commands: Some(commands),
                events: DesktopRuntimeEventStream {
                    priority_updates,
                    data_updates,
                    pending_priority_update: None,
                    pending_data_update: None,
                },
                shutdown: DesktopRuntimeShutdownGuard {
                    shutdown,
                    runtime_thread: Some(runtime_thread),
                },
            }),
        })
    }

    #[cfg(test)]
    pub fn try_reload(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Reload { command_id })
    }

    #[cfg(test)]
    pub fn try_resync(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Resync { command_id })
    }

    #[cfg(test)]
    pub fn try_create_session(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::CreateSession { command_id })
    }

    #[cfg(test)]
    pub fn try_open_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::OpenSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn try_list_sessions(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::ListSessions { command_id })
    }

    #[cfg(test)]
    pub fn try_select_model(
        &self,
        command_id: u64,
        model_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_selection_id("model", model_id)?;
        self.try_send(DesktopRuntimeCommand::SelectModel {
            command_id,
            model_id: model_id.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn try_select_session_profile(
        &self,
        command_id: u64,
        profile_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_selection_id("profile", profile_id)?;
        self.try_send(DesktopRuntimeCommand::SelectSessionProfile {
            command_id,
            profile_id: profile_id.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn try_submit_prompt(
        &self,
        command_id: u64,
        prompt: &str,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_prompt(prompt)?;
        self.try_send(DesktopRuntimeCommand::SubmitPrompt {
            command_id,
            prompt: prompt.to_owned(),
            thinking_level,
        })
    }

    #[cfg(test)]
    pub fn try_abort(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Abort { command_id })
    }

    #[cfg(test)]
    pub fn try_steer(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::Steer {
            command_id,
            text: text.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn try_follow_up(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::FollowUp {
            command_id,
            text: text.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn try_decide_tool_authorization(
        &self,
        command_id: u64,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_authorization_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::DecideToolAuthorization {
            command_id,
            identity: identity.clone(),
            decision,
        })
    }

    #[cfg(test)]
    pub fn try_retry_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::RetryRecovery {
            command_id,
            identity: identity.clone(),
        })
    }

    #[cfg(test)]
    pub fn try_resolve_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::ResolveRecovery {
            command_id,
            identity: identity.clone(),
            resolution,
        })
    }

    pub fn into_parts(
        mut self,
    ) -> (
        DesktopRuntimeCommandHandle,
        DesktopRuntimeEventStream,
        DesktopRuntimeShutdownGuard,
    ) {
        let commands = self
            .commands
            .take()
            .expect("live desktop bridge must retain its command sender");
        (
            DesktopRuntimeCommandHandle { commands },
            self.events,
            self.shutdown,
        )
    }

    #[cfg(test)]
    pub async fn next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        self.events.next_update().await
    }

    #[cfg(test)]
    pub async fn next_update_batch(&mut self) -> Option<Vec<DesktopRuntimeUpdate>> {
        self.events.next_update_batch().await
    }

    #[cfg(test)]
    pub async fn shutdown(mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.shutdown.signal();
        drop(self.commands.take());
        while let Some(update) = self.events.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                break;
            }
        }
        self.shutdown.join()
    }

    #[cfg(test)]
    fn try_send(&self, command: DesktopRuntimeCommand) -> Result<(), DesktopCommandAdmissionError> {
        self.commands
            .as_ref()
            .ok_or(DesktopCommandAdmissionError::RuntimeClosed)?
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DesktopCommandAdmissionError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => DesktopCommandAdmissionError::RuntimeClosed,
            })
    }

    fn join_runtime_thread(&mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.shutdown.join()
    }
}

#[cfg(test)]
pub(crate) struct DesktopRuntimeTestHarness {
    commands: mpsc::Receiver<DesktopRuntimeCommand>,
    _priority_update_tx: mpsc::Sender<DesktopRuntimeUpdate>,
    _data_update_tx: mpsc::Sender<DesktopRuntimeUpdate>,
}

#[cfg(test)]
impl DesktopRuntimeTestHarness {
    pub(crate) fn drain_command_kinds(&mut self) -> Vec<DesktopRuntimeCommandKind> {
        let mut kinds = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            kinds.push(command.kind());
        }
        kinds
    }
}

impl DesktopRuntimeEventStream {
    pub async fn next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        loop {
            if let Some(update) = self.try_next_update() {
                return Some(update);
            }
            if self.priority_updates.is_closed() && self.data_updates.is_closed() {
                return None;
            }
            tokio::select! {
                biased;
                update = self.priority_updates.recv(), if !self.priority_updates.is_closed() => {
                    self.pending_priority_update = update;
                }
                update = self.data_updates.recv(), if !self.data_updates.is_closed() => {
                    self.pending_data_update = update;
                }
            }
        }
    }

    /// Await one lossless delivery batch.
    ///
    /// Only ProductEvent data updates open the bounded coalescing window.
    /// Control, recovery, terminal, failure, and shutdown updates return
    /// immediately, including when they interrupt an active data batch.
    pub async fn next_update_batch(&mut self) -> Option<Vec<DesktopRuntimeUpdate>> {
        let first = self.next_update().await?;
        if !is_streaming_data_update(&first) {
            return Some(vec![first]);
        }
        let mut updates = Vec::with_capacity(MAX_STREAMING_DELIVERIES_PER_BATCH);
        updates.push(first);
        // This future is polled by GPUI's executor in production, so it must
        // not assume that a Tokio reactor is entered on the UI thread.
        let deadline = gpui::Timer::after(STREAMING_DELIVERY_COALESCE_WINDOW);
        tokio::pin!(deadline);
        while updates.len() < MAX_STREAMING_DELIVERIES_PER_BATCH {
            tokio::select! {
                biased;
                update = self.next_update() => {
                    let Some(update) = update else {
                        break;
                    };
                    let immediate = !is_streaming_data_update(&update);
                    updates.push(update);
                    if immediate {
                        break;
                    }
                }
                _ = &mut deadline => break,
            }
        }
        Some(updates)
    }

    pub fn try_next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        if self.pending_priority_update.is_none() {
            self.pending_priority_update = self.priority_updates.try_recv().ok();
        }
        if self.pending_data_update.is_none() {
            self.pending_data_update = self.data_updates.try_recv().ok();
        }
        self.take_next_pending_update()
    }

    fn take_next_pending_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        match (
            self.pending_priority_update.as_ref(),
            self.pending_data_update.as_ref(),
        ) {
            (Some(priority), Some(data)) if data_precedes_priority(data, priority) => {
                self.pending_data_update.take()
            }
            (Some(_), _) => self.pending_priority_update.take(),
            (None, Some(_)) => self.pending_data_update.take(),
            (None, None) => None,
        }
    }
}

impl DesktopRuntimeShutdownGuard {
    fn signal(&self) {
        let _ = self.shutdown.send(true);
    }

    pub async fn shutdown(
        mut self,
        events: &mut DesktopRuntimeEventStream,
    ) -> Result<(), DesktopRuntimeShutdownError> {
        self.signal();
        while let Some(update) = events.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                break;
            }
        }
        self.join()
    }

    fn join(&mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.runtime_thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .map_err(|_| DesktopRuntimeShutdownError::RuntimePanicked)
        })
    }
}

impl DesktopRuntimeCommandHandle {
    pub fn try_reload(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Reload { command_id })
    }

    pub fn try_resync(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Resync { command_id })
    }

    pub fn try_create_session(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::CreateSession { command_id })
    }

    pub fn try_open_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::OpenSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_list_sessions(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::ListSessions { command_id })
    }

    pub fn try_select_model(
        &self,
        command_id: u64,
        model_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_selection_id("model", model_id)?;
        self.try_send(DesktopRuntimeCommand::SelectModel {
            command_id,
            model_id: model_id.to_owned(),
        })
    }

    pub fn try_select_session_profile(
        &self,
        command_id: u64,
        profile_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_selection_id("profile", profile_id)?;
        self.try_send(DesktopRuntimeCommand::SelectSessionProfile {
            command_id,
            profile_id: profile_id.to_owned(),
        })
    }

    pub fn try_submit_prompt(
        &self,
        command_id: u64,
        prompt: &str,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_prompt(prompt)?;
        self.try_send(DesktopRuntimeCommand::SubmitPrompt {
            command_id,
            prompt: prompt.to_owned(),
            thinking_level,
        })
    }

    pub fn try_abort(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Abort { command_id })
    }

    pub fn try_steer(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::Steer {
            command_id,
            text: text.to_owned(),
        })
    }

    pub fn try_follow_up(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::FollowUp {
            command_id,
            text: text.to_owned(),
        })
    }

    pub fn try_decide_tool_authorization(
        &self,
        command_id: u64,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_authorization_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::DecideToolAuthorization {
            command_id,
            identity: identity.clone(),
            decision,
        })
    }

    pub fn try_retry_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::RetryRecovery {
            command_id,
            identity: identity.clone(),
        })
    }

    pub fn try_resolve_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::ResolveRecovery {
            command_id,
            identity: identity.clone(),
            resolution,
        })
    }

    pub fn try_review_changed_file(
        &self,
        command_id: u64,
        request: &CodingAgentFileReviewRequest,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_file_review_request(request)?;
        self.try_send(DesktopRuntimeCommand::ReviewChangedFile {
            command_id,
            request: request.clone(),
        })
    }

    pub fn try_open_external_editor(
        &self,
        command_id: u64,
        target: &CodingAgentExternalEditorTarget,
        editor: &DesktopExternalEditorConfig,
    ) -> Result<(), DesktopCommandAdmissionError> {
        editor.validate().map_err(
            |error| DesktopCommandAdmissionError::InvalidExternalEditor {
                message: error.to_string(),
            },
        )?;
        self.try_send(DesktopRuntimeCommand::OpenExternalEditor {
            command_id,
            target: target.clone(),
            editor: editor.clone(),
        })
    }

    fn try_send(&self, command: DesktopRuntimeCommand) -> Result<(), DesktopCommandAdmissionError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DesktopCommandAdmissionError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => DesktopCommandAdmissionError::RuntimeClosed,
            })
    }
}

fn data_precedes_priority(data: &DesktopRuntimeUpdate, priority: &DesktopRuntimeUpdate) -> bool {
    match (
        product_event_sequence(data),
        product_event_sequence(priority),
    ) {
        (Some(data_sequence), Some(priority_sequence)) => data_sequence < priority_sequence,
        (Some(_), None)
            if matches!(
                priority,
                DesktopRuntimeUpdate::PromptFinished { .. } | DesktopRuntimeUpdate::Stopped
            ) =>
        {
            true
        }
        _ => false,
    }
}

fn product_event_sequence(update: &DesktopRuntimeUpdate) -> Option<u64> {
    match update {
        DesktopRuntimeUpdate::ProductEvent { event } => Some(event.sequence()),
        _ => None,
    }
}

fn is_streaming_data_update(update: &DesktopRuntimeUpdate) -> bool {
    matches!(
        update,
        DesktopRuntimeUpdate::ProductEvent { event }
            if event.delivery_class() == CodingAgentProductEventDeliveryClass::Data
    )
}

impl Drop for DesktopRuntimeShutdownGuard {
    fn drop(&mut self) {
        self.signal();
        if let Some(runtime_thread) = self.runtime_thread.take() {
            spawn_runtime_reaper("desktop-runtime-reaper", runtime_thread);
        }
    }
}

fn spawn_runtime_reaper(name: &str, runtime_thread: JoinHandle<()>) {
    let _ = thread::Builder::new().name(name.into()).spawn(move || {
        let _ = runtime_thread.join();
    });
}

struct RuntimeState {
    context: CodingAgentEmbeddingContext,
    session: Option<CodingAgentSession>,
}

impl RuntimeState {
    fn metadata_snapshot(&self) -> Result<DesktopRuntimeMetadataSnapshot, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        Ok(DesktopRuntimeMetadataSnapshot {
            project: self.context.snapshot().clone(),
            session: session.snapshot(),
        })
    }

    fn snapshot(&self) -> Result<DesktopRuntimeHydratedSnapshot, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        Ok(DesktopRuntimeHydratedSnapshot {
            project: self.context.snapshot().clone(),
            session: session.snapshot(),
            transcript: session.transcript_snapshot()?,
            pending_recoveries: session.recovery_pending()?,
        })
    }

    fn session_catalog(
        &self,
    ) -> Result<(Vec<DesktopSessionCatalogEntry>, usize), DesktopBridgeError> {
        let summaries = self.context.list_sessions()?;
        let omitted = summaries.len().saturating_sub(MAX_DESKTOP_SESSION_CATALOG);
        let sessions = summaries
            .into_iter()
            .take(MAX_DESKTOP_SESSION_CATALOG)
            .map(|summary| DesktopSessionCatalogEntry {
                session_id: bounded_utf8_prefix(&summary.session_id, MAX_SESSION_ID_BYTES),
                updated_at: bounded_utf8_prefix(&summary.updated_at, 128),
            })
            .collect();
        Ok((sessions, omitted))
    }

    fn recovery_snapshot(&self) -> Result<DesktopRuntimeRecoverySnapshot, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        Ok(DesktopRuntimeRecoverySnapshot {
            project: self.context.snapshot().clone(),
            session: session.snapshot(),
            pending_recoveries: session.recovery_pending()?,
        })
    }

    async fn review_changed_file(
        &self,
        request: CodingAgentFileReviewRequest,
    ) -> Result<CodingAgentFileReview, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        session
            .review_changed_file(request)
            .await
            .map_err(DesktopBridgeError::from)
    }

    async fn open_external_editor(
        &self,
        target: CodingAgentExternalEditorTarget,
        editor: DesktopExternalEditorConfig,
    ) -> Result<String, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        session.revalidate_external_editor_target(&target).await?;
        let project_relative_path = target.project_relative_path().to_owned();
        task::spawn_blocking(move || launch_external_editor(&editor, &target))
            .await
            .map_err(|_| DesktopBridgeError::ExternalEditor)??;
        Ok(project_relative_path)
    }

    fn retry_recovery(
        &mut self,
        identity: DesktopRecoveryIdentity,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        let result = session.retry_recovery(CodingAgentRecoveryRetryRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            schedule_with_backoff: false,
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot()?))
    }

    fn resolve_recovery(
        &mut self,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let action = match resolution {
            CodingAgentRecoveryResolution::Failed => "marked failed",
            CodingAgentRecoveryResolution::Aborted => "aborted",
        };
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        let result = session.resolve_recovery(CodingAgentRecoveryResolutionRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            resolution,
            reason: format!("native desktop operator {action} uncertain operation"),
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot()?))
    }

    async fn select_session_profile(
        &mut self,
        profile_id: String,
    ) -> Result<DesktopRuntimeMetadataSnapshot, DesktopBridgeError> {
        if !self
            .context
            .snapshot()
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == profile_id)
        {
            return Err(DesktopBridgeError::Input {
                message: format!("unknown desktop session profile {profile_id}"),
            });
        }
        let profile_id =
            ProfileId::new(profile_id).map_err(|message| DesktopBridgeError::Input {
                message: format!("invalid desktop session profile: {message}"),
            })?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_profile_selection".into(),
            })?;
        let outcome = session
            .run(CodingAgentOperation::SetDefaultAgentProfile { profile_id })
            .await?;
        if !matches!(
            outcome,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ) {
            return Err(DesktopBridgeError::Session {
                message: "desktop profile selection returned an unexpected outcome".into(),
            });
        }
        self.metadata_snapshot()
    }

    async fn replace_with_new_session(&mut self) -> Result<(), DesktopBridgeError> {
        let replacement = self.context.create_session().await?;
        self.shutdown_idle_session().await?;
        self.session = Some(replacement);
        Ok(())
    }

    async fn replace_with_open_session(
        &mut self,
        session_id: String,
    ) -> Result<(), DesktopBridgeError> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.view().session_id == session_id)
        {
            return Ok(());
        }
        let replacement = self.context.open_session(session_id).await?;
        self.shutdown_idle_session().await?;
        self.session = Some(replacement);
        Ok(())
    }

    fn start_prompt(
        &mut self,
        command_id: u64,
        prompt: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<ActivePrompt, DesktopBridgeError> {
        let mut session = self
            .session
            .take()
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_prompt".into(),
            })?;
        let connection = match session.connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID)) {
            Ok(connection) => connection,
            Err(error) => {
                self.session = Some(session);
                return Err(error.into());
            }
        };
        let operation = self
            .context
            .prompt_operation(PromptInvocation::Text(prompt.clone()), thinking_level);
        let draft_id = CodingAgentDraftId(format!("desktop-prompt-{command_id}"));
        let submission = match connection.prepare_client_submission(
            &mut session,
            Some(CodingAgentSubmissionDraft::new(draft_id, prompt)),
            operation,
        ) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = connection.detach();
                self.session = Some(session);
                return Err(error.into());
            }
        };
        let requested_after = match connection.state() {
            Ok(snapshot) => snapshot.cursor.last_event_sequence,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.session = Some(session);
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error.into());
            }
        };
        let (events, pending_recovery) = match reconnect_event_source(&connection, requested_after)
        {
            Ok(reconnect) => reconnect,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.session = Some(session);
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error);
            }
        };
        let project = self.context.snapshot().clone();
        let task = task::spawn(async move {
            let result = submission
                .run(&mut session)
                .await
                .map_err(DesktopBridgeError::from);
            (session, result)
        });
        Ok(ActivePrompt {
            command_id,
            operation_id: None,
            project,
            connection,
            events,
            pending_recovery,
            last_forwarded_sequence: requested_after,
            task,
        })
    }

    async fn shutdown_idle_session(&mut self) -> Result<(), DesktopBridgeError> {
        if let Some(mut session) = self.session.take() {
            session.shutdown().await?;
        }
        Ok(())
    }
}

type PromptTaskOutput = (
    CodingAgentSession,
    Result<CodingAgentOperationOutcome, DesktopBridgeError>,
);

struct ActivePrompt {
    command_id: u64,
    operation_id: Option<String>,
    project: CodingAgentEmbeddingSnapshot,
    connection: CodingAgentClientConnection,
    events: DesktopProductEventSource,
    pending_recovery: Option<CodingAgentFreshSnapshotRecovery>,
    last_forwarded_sequence: u64,
    task: task::JoinHandle<PromptTaskOutput>,
}

enum ActiveSignal {
    Command(Option<DesktopRuntimeCommand>),
    Event(Box<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
    Finished(Box<Result<PromptTaskOutput, task::JoinError>>),
    Shutdown,
}

async fn run_runtime(
    options: CodingAgentEmbeddingOptions,
    mut commands: mpsc::Receiver<DesktopRuntimeCommand>,
    mut shutdown: watch::Receiver<bool>,
    priority_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    ready: std_mpsc::SyncSender<Result<DesktopRuntimeHydratedSnapshot, DesktopRuntimeError>>,
) {
    let context = match CodingAgentEmbeddingContext::load(options) {
        Ok(context) => context,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            return;
        }
    };
    let session = match context.create_session().await {
        Ok(session) => session,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            return;
        }
    };
    let mut state = RuntimeState {
        context,
        session: Some(session),
    };
    let initial = match state.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            let _ = state.shutdown_idle_session().await;
            return;
        }
    };
    if ready.send(Ok(initial)).is_err() {
        let _ = state.shutdown_idle_session().await;
        return;
    }

    let mut active: Option<ActivePrompt> = None;
    loop {
        if let Some(active_prompt) = active.as_mut() {
            if let Some(recovery) = active_prompt.pending_recovery.take() {
                active_prompt.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    shutdown_active_prompt(active.take(), &priority_updates).await;
                    return;
                }
            }
            let signal = tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    ActiveSignal::Shutdown
                }
                command = commands.recv() => ActiveSignal::Command(command),
                result = &mut active_prompt.task => ActiveSignal::Finished(Box::new(result)),
                event = recv_product_event(&mut active_prompt.events) => {
                    ActiveSignal::Event(Box::new(event))
                },
            };
            match signal {
                ActiveSignal::Shutdown | ActiveSignal::Command(None) => {
                    shutdown_active_prompt(active.take(), &priority_updates).await;
                    break;
                }
                ActiveSignal::Command(Some(command)) => {
                    let update = handle_active_command(
                        active.as_ref().expect("active prompt exists"),
                        command,
                    );
                    if priority_updates.send(update).await.is_err() {
                        shutdown_active_prompt(active.take(), &priority_updates).await;
                        return;
                    }
                }
                ActiveSignal::Event(event) => match *event {
                    Ok(CodingAgentReconnectDelivery::Event(event)) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        let sequence = event.sequence();
                        let candidate_operation_id = event.operation_id().map(str::to_owned);
                        if !ensure_operation_started(
                            active_prompt,
                            candidate_operation_id.as_deref(),
                            &priority_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        if !publish_product_event(
                            event,
                            active_prompt,
                            &priority_updates,
                            &data_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        if !acknowledge_product_event(active_prompt, sequence, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        active_prompt.last_forwarded_sequence = sequence;
                    }
                    Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        active_prompt.last_forwarded_sequence =
                            recovery.fresh_cursor.last_event_sequence;
                        if priority_updates
                            .send(recovery_update(recovery))
                            .await
                            .is_err()
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                    }
                    Err(error) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        if !recover_product_event_source(active_prompt, error, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                    }
                },
                ActiveSignal::Finished(result) => {
                    let result = *result;
                    let mut completed = active.take().expect("active prompt exists");
                    if !drain_product_events(&mut completed, &priority_updates, &data_updates).await
                    {
                        shutdown_active_prompt(Some(completed), &priority_updates).await;
                        return;
                    }
                    let _ = completed.connection.detach();
                    match result {
                        Ok((session, operation_result)) => {
                            state.session = Some(session);
                            if !ensure_operation_started(&mut completed, None, &priority_updates)
                                .await
                            {
                                break;
                            }
                            let Some(operation_id) = completed.operation_id.take() else {
                                let _ = priority_updates
                                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                                        error: DesktopRuntimeError {
                                            code: "operation_association_missing".into(),
                                            message: "completed desktop prompt has no product operation id"
                                                .into(),
                                        },
                                    })
                                    .await;
                                break;
                            };
                            let snapshot = match state.snapshot() {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    let _ = priority_updates
                                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                                            error: runtime_error(&error),
                                        })
                                        .await;
                                    break;
                                }
                            };
                            let error = operation_result.err().map(|error| runtime_error(&error));
                            if priority_updates
                                .send(DesktopRuntimeUpdate::PromptFinished {
                                    command_id: completed.command_id,
                                    operation_id,
                                    snapshot,
                                    error,
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = priority_updates
                                .send(DesktopRuntimeUpdate::RuntimeFailed {
                                    error: local_runtime_error(
                                        "runtime_task_panicked",
                                        "A desktop runtime task stopped unexpectedly.",
                                    ),
                                })
                                .await;
                            break;
                        }
                    }
                }
            }
            continue;
        }

        let command = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                let _ = changed;
                None
            }
            command = commands.recv() => command,
        };
        let Some(command) = command else {
            break;
        };
        let command_id = command.command_id();
        let kind = command.kind();
        let result = match command {
            DesktopRuntimeCommand::Reload { .. } => {
                let reload = state
                    .context
                    .reload_local_resources()
                    .map(|_| ())
                    .map_err(DesktopBridgeError::from);
                reload
                    .and_then(|()| state.metadata_snapshot())
                    .map(|metadata| DesktopRuntimeUpdate::Reloaded {
                        command_id,
                        metadata,
                    })
            }
            DesktopRuntimeCommand::Resync { .. } => {
                state
                    .snapshot()
                    .map(|snapshot| DesktopRuntimeUpdate::Resynced {
                        command_id,
                        replacement: DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
                    })
            }
            DesktopRuntimeCommand::CreateSession { .. } => state
                .replace_with_new_session()
                .await
                .and_then(|()| state.snapshot())
                .map(|snapshot| DesktopRuntimeUpdate::SessionChanged {
                    command_id,
                    snapshot,
                }),
            DesktopRuntimeCommand::OpenSession { session_id, .. } => state
                .replace_with_open_session(session_id)
                .await
                .and_then(|()| state.snapshot())
                .map(|snapshot| DesktopRuntimeUpdate::SessionChanged {
                    command_id,
                    snapshot,
                }),
            DesktopRuntimeCommand::ListSessions { .. } => {
                state.session_catalog().map(|(sessions, omitted)| {
                    DesktopRuntimeUpdate::SessionsListed {
                        command_id,
                        sessions,
                        omitted,
                    }
                })
            }
            DesktopRuntimeCommand::SelectModel { model_id, .. } => state
                .context
                .select_model(model_id)
                .map(|_| ())
                .map_err(DesktopBridgeError::from)
                .and_then(|()| state.metadata_snapshot())
                .map(|metadata| DesktopRuntimeUpdate::SelectionChanged {
                    command_id,
                    selection: DesktopRuntimeSelectionKind::Model,
                    metadata,
                }),
            DesktopRuntimeCommand::SelectSessionProfile { profile_id, .. } => state
                .select_session_profile(profile_id)
                .await
                .map(|metadata| DesktopRuntimeUpdate::SelectionChanged {
                    command_id,
                    selection: DesktopRuntimeSelectionKind::SessionProfile,
                    metadata,
                }),
            DesktopRuntimeCommand::RetryRecovery { identity, .. } => state
                .retry_recovery(identity)
                .map(
                    |(recovery_id, recovery)| DesktopRuntimeUpdate::RecoveryChanged {
                        command_id,
                        action: DesktopRecoveryAction::Retry,
                        recovery_id,
                        recovery,
                    },
                ),
            DesktopRuntimeCommand::ResolveRecovery {
                identity,
                resolution,
                ..
            } => {
                let action = match resolution {
                    CodingAgentRecoveryResolution::Failed => DesktopRecoveryAction::MarkFailed,
                    CodingAgentRecoveryResolution::Aborted => DesktopRecoveryAction::Abort,
                };
                state
                    .resolve_recovery(identity, resolution)
                    .map(
                        |(recovery_id, recovery)| DesktopRuntimeUpdate::RecoveryChanged {
                            command_id,
                            action,
                            recovery_id,
                            recovery,
                        },
                    )
            }
            DesktopRuntimeCommand::ReviewChangedFile { request, .. } => state
                .review_changed_file(request)
                .await
                .map(|review| DesktopRuntimeUpdate::FileReviewed { command_id, review }),
            DesktopRuntimeCommand::OpenExternalEditor { target, editor, .. } => state
                .open_external_editor(target, editor)
                .await
                .map(
                    |project_relative_path| DesktopRuntimeUpdate::ExternalEditorOpened {
                        command_id,
                        project_relative_path,
                    },
                ),
            DesktopRuntimeCommand::SubmitPrompt {
                prompt,
                thinking_level,
                ..
            } => match state.start_prompt(command_id, prompt, thinking_level) {
                Ok(started) => {
                    active = Some(started);
                    Ok(DesktopRuntimeUpdate::PromptAccepted { command_id })
                }
                Err(error) => Err(error),
            },
            DesktopRuntimeCommand::Abort { .. }
            | DesktopRuntimeCommand::Steer { .. }
            | DesktopRuntimeCommand::FollowUp { .. }
            | DesktopRuntimeCommand::DecideToolAuthorization { .. } => {
                Err(DesktopBridgeError::Busy {
                    operation: "no_active_prompt".into(),
                })
            }
        };
        let update = result.unwrap_or_else(|error| {
            let error = runtime_error(&error);
            DesktopRuntimeUpdate::CommandRejected {
                command_id,
                command: kind,
                code: error.code,
                message: error.message,
            }
        });
        if priority_updates.send(update).await.is_err() {
            break;
        }
    }

    let _ = state.shutdown_idle_session().await;
    let _ = priority_updates.send(DesktopRuntimeUpdate::Stopped).await;
}

fn handle_active_command(
    active: &ActivePrompt,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    let command_id = command.command_id();
    let kind = command.kind();
    if matches!(command, DesktopRuntimeCommand::Resync { .. }) {
        return match active.connection.state() {
            Ok(session) => DesktopRuntimeUpdate::Resynced {
                command_id,
                replacement: DesktopRuntimeResyncSnapshot::Metadata(
                    DesktopRuntimeMetadataSnapshot {
                        project: active.project.clone(),
                        session,
                    },
                ),
            },
            Err(error) => {
                let error = runtime_error(&error);
                DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: kind,
                    code: error.code,
                    message: error.message,
                }
            }
        };
    }
    let Some(operation_id) = active.operation_id.as_deref() else {
        return DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command: kind,
            code: "operation_starting".into(),
            message: "desktop prompt has not received its product operation identity yet".into(),
        };
    };
    if let DesktopRuntimeCommand::DecideToolAuthorization {
        identity, decision, ..
    } = command
    {
        return match active
            .connection
            .decide_tool_authorization(&identity, decision.clone())
        {
            Ok(()) => DesktopRuntimeUpdate::AuthorizationDecisionAccepted {
                command_id,
                authorization_id: identity.authorization_id,
                decision,
            },
            Err(error) => {
                let error = runtime_error(&error);
                DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: kind,
                    code: error.code,
                    message: error.message,
                }
            }
        };
    }
    let control = active.connection.prompt_control(operation_id);
    let control_id = CodingAgentControlId(format!("desktop-control-{command_id}"));
    let result = match command {
        DesktopRuntimeCommand::Abort { .. } => {
            control.abort(control_id, "desktop user requested abort")
        }
        DesktopRuntimeCommand::Steer { text, .. } => control.steer(control_id, text),
        DesktopRuntimeCommand::FollowUp { text, .. } => control.follow_up(control_id, text),
        DesktopRuntimeCommand::Reload { .. }
        | DesktopRuntimeCommand::Resync { .. }
        | DesktopRuntimeCommand::CreateSession { .. }
        | DesktopRuntimeCommand::OpenSession { .. }
        | DesktopRuntimeCommand::ListSessions { .. }
        | DesktopRuntimeCommand::SelectModel { .. }
        | DesktopRuntimeCommand::SelectSessionProfile { .. }
        | DesktopRuntimeCommand::SubmitPrompt { .. }
        | DesktopRuntimeCommand::DecideToolAuthorization { .. }
        | DesktopRuntimeCommand::RetryRecovery { .. }
        | DesktopRuntimeCommand::ResolveRecovery { .. }
        | DesktopRuntimeCommand::ReviewChangedFile { .. }
        | DesktopRuntimeCommand::OpenExternalEditor { .. } => {
            return DesktopRuntimeUpdate::CommandRejected {
                command_id,
                command: kind,
                code: "busy".into(),
                message: format!(
                    "desktop runtime is executing prompt operation {}",
                    operation_id
                ),
            };
        }
    };
    match result {
        Ok(receipt) => DesktopRuntimeUpdate::ControlAccepted {
            command_id,
            command: kind,
            receipt,
        },
        Err(rejection) => DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command: kind,
            code: "control_rejected".into(),
            message: format!(
                "control {:?} for operation {} was rejected: {:?}",
                rejection.kind, rejection.operation_id, rejection.reason
            ),
        },
    }
}

async fn recv_product_event(
    receiver: &mut DesktopProductEventSource,
) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
    receiver.recv().await
}

struct DesktopProductEventSource {
    replay: VecDeque<CodingAgentProductEvent>,
    receiver: DesktopProductEventReceiver,
}

enum DesktopProductEventReceiver {
    Product(CodingAgentReconnectReceiver),
    #[cfg(test)]
    Injected(mpsc::Receiver<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
}

impl DesktopProductEventReceiver {
    async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.recv().await.map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => receiver
                .recv()
                .await
                .unwrap_or_else(|| Err(DesktopBridgeError::cancelled_for_tests())),
        }
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.try_recv().map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => match receiver.try_recv() {
                Ok(delivery) => delivery.map(Some),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    Err(DesktopBridgeError::cancelled_for_tests())
                }
            },
        }
    }
}

impl DesktopProductEventSource {
    async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(CodingAgentReconnectDelivery::Event(event));
        }
        self.receiver.recv().await
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(CodingAgentReconnectDelivery::Event(event)));
        }
        self.receiver.try_recv()
    }
}

enum DesktopReconnectAttempt<R> {
    Replayed {
        events: Vec<CodingAgentProductEvent>,
        receiver: R,
    },
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

fn establish_reconnect<R>(
    requested_after: u64,
    mut reconnect: impl FnMut(u64) -> Result<DesktopReconnectAttempt<R>, DesktopBridgeError>,
) -> Result<
    (
        Vec<CodingAgentProductEvent>,
        R,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    match reconnect(requested_after)? {
        DesktopReconnectAttempt::Replayed { events, receiver } => Ok((events, receiver, None)),
        DesktopReconnectAttempt::FreshSnapshotRequired(recovery) => {
            let fresh_sequence = recovery.fresh_cursor.last_event_sequence;
            match reconnect(fresh_sequence)? {
                DesktopReconnectAttempt::Replayed { events, receiver } => {
                    Ok((events, receiver, Some(recovery)))
                }
                DesktopReconnectAttempt::FreshSnapshotRequired(second) => {
                    Err(DesktopBridgeError::Input {
                        message: format!(
                            "desktop ProductEvent reconnect exhausted after fresh cursor {} \
                             (oldest retained sequence {})",
                            second.requested_sequence, second.oldest_available_sequence
                        ),
                    })
                }
            }
        }
    }
}

fn reconnect_event_source(
    connection: &CodingAgentClientConnection,
    requested_after: u64,
) -> Result<
    (
        DesktopProductEventSource,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    let (events, receiver, recovery) = establish_reconnect(requested_after, |sequence| {
        connection
            .reconnect(sequence)
            .map(|reconnect| match reconnect {
                CodingAgentReconnect::Replayed {
                    events, receiver, ..
                } => DesktopReconnectAttempt::Replayed { events, receiver },
                CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
                    DesktopReconnectAttempt::FreshSnapshotRequired(recovery)
                }
            })
            .map_err(DesktopBridgeError::from)
    })?;
    Ok((
        DesktopProductEventSource {
            replay: events.into(),
            receiver: DesktopProductEventReceiver::Product(receiver),
        },
        recovery,
    ))
}

async fn recover_product_event_source(
    active: &mut ActivePrompt,
    receiver_error: DesktopBridgeError,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match reconnect_event_source(&active.connection, active.last_forwarded_sequence) {
        Ok((events, recovery)) => {
            active.events = events;
            if let Some(recovery) = recovery {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_ok()
            } else {
                true
            }
        }
        Err(reconnect_error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: DesktopRuntimeError {
                    code: "product_event_reconnect_failed".into(),
                    message: format!(
                        "ProductEvent receiver failed ({}); reconnect from sequence {} failed: {}",
                        receiver_error, active.last_forwarded_sequence, reconnect_error
                    ),
                },
            })
            .await
            .is_ok(),
    }
}

fn recovery_update(recovery: CodingAgentFreshSnapshotRecovery) -> DesktopRuntimeUpdate {
    let reason = match recovery.reason {
        CodingAgentRecoveryReason::RetainedHistoryGap => DesktopRuntimeError {
            code: "product_event_retained_history_gap".into(),
            message: format!(
                "ProductEvent replay after sequence {} is unavailable; oldest retained sequence is {}",
                recovery.requested_sequence, recovery.oldest_available_sequence
            ),
        },
        CodingAgentRecoveryReason::LiveReceiverLag => DesktopRuntimeError {
            code: "product_event_live_receiver_lag".into(),
            message: format!(
                "ProductEvent receiver lagged after sequence {}; recovered at fresh sequence {}",
                recovery.requested_sequence, recovery.fresh_cursor.last_event_sequence
            ),
        },
    };
    DesktopRuntimeUpdate::ResyncRequired {
        reason,
        snapshot: *recovery.snapshot,
    }
}

async fn publish_product_event(
    event: CodingAgentProductEvent,
    active: &ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if event.family() == CodingAgentProductEventFamily::Capability {
        let snapshot = match active.connection.state() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return priority_updates
                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                        error: runtime_error(&error),
                    })
                    .await
                    .is_ok();
            }
        };
        return priority_updates
            .send(DesktopRuntimeUpdate::ResyncRequired {
                reason: DesktopRuntimeError {
                    code: "capability_generation_changed".into(),
                    message: format!(
                        "capability generation changed at ProductEvent sequence {}; replacing the desktop projection atomically",
                        event.sequence()
                    ),
                },
                snapshot,
            })
            .await
            .is_ok();
    }
    if is_priority_event(&event) {
        return priority_updates
            .send(DesktopRuntimeUpdate::ProductEvent { event })
            .await
            .is_ok();
    }
    publish_data_update(
        DesktopRuntimeUpdate::ProductEvent { event },
        || active.connection.state(),
        priority_updates,
        data_updates,
    )
    .await
}

async fn acknowledge_product_event(
    active: &ActivePrompt,
    sequence: u64,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match active.connection.acknowledge(sequence) {
        Ok(_) => true,
        Err(error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: runtime_error(&error),
            })
            .await
            .is_ok(),
    }
}

async fn publish_data_update<E>(
    update: DesktopRuntimeUpdate,
    snapshot: impl FnOnce() -> Result<CodingAgentSnapshot, E>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool
where
    E: DesktopRuntimeErrorSource,
{
    match data_updates.try_send(update) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
        Err(mpsc::error::TrySendError::Full(_)) => {
            let snapshot = match snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return priority_updates
                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                            error: runtime_error(&error),
                        })
                        .await
                        .is_ok();
                }
            };
            priority_updates
                .send(DesktopRuntimeUpdate::ResyncRequired {
                    reason: DesktopRuntimeError {
                        code: "desktop_data_queue_full".into(),
                        message: format!(
                            "desktop message update queue reached its {}-event bound",
                            DESKTOP_UPDATE_QUEUE_CAPACITY
                        ),
                    },
                    snapshot,
                })
                .await
                .is_ok()
        }
    }
}

async fn ensure_operation_started(
    active: &mut ActivePrompt,
    candidate_operation_id: Option<&str>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if active.operation_id.is_some() {
        return true;
    }
    let snapshot = match active.connection.state() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: runtime_error(&error),
                })
                .await;
            return false;
        }
    };
    let operation_id = snapshot
        .submitted_operation
        .as_ref()
        .map(|operation| operation.operation_id.clone())
        .or_else(|| candidate_operation_id.map(str::to_owned));
    let Some(operation_id) = operation_id else {
        return true;
    };
    active.operation_id = Some(operation_id.clone());
    priority_updates
        .send(DesktopRuntimeUpdate::PromptStarted {
            command_id: active.command_id,
            operation_id,
            metadata: DesktopRuntimeMetadataSnapshot {
                project: active.project.clone(),
                session: snapshot,
            },
        })
        .await
        .is_ok()
}

async fn drain_product_events(
    active: &mut ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    loop {
        let received = active.events.try_recv();
        match received {
            Ok(Some(CodingAgentReconnectDelivery::Event(event))) => {
                let sequence = event.sequence();
                let candidate_operation_id = event.operation_id().map(str::to_owned);
                if !ensure_operation_started(
                    active,
                    candidate_operation_id.as_deref(),
                    priority_updates,
                )
                .await
                {
                    return false;
                }
                if !publish_product_event(event, active, priority_updates, data_updates).await {
                    return false;
                }
                if !acknowledge_product_event(active, sequence, priority_updates).await {
                    return false;
                }
                active.last_forwarded_sequence = sequence;
            }
            Ok(Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery))) => {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            Ok(None) => return true,
            Err(error) => {
                return recover_product_event_source(active, error, priority_updates).await;
            }
        }
    }
}

fn is_priority_event(event: &CodingAgentProductEvent) -> bool {
    !matches!(
        (event.delivery_class(), event.family()),
        (
            CodingAgentProductEventDeliveryClass::Data,
            CodingAgentProductEventFamily::Message
        )
    )
}

async fn shutdown_active_prompt(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    shutdown_active_prompt_with_deadline(active, priority_updates, RUNTIME_SHUTDOWN_DEADLINE).await;
}

async fn shutdown_active_prompt_with_deadline(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    shutdown_deadline: Duration,
) {
    let Some(mut active) = active else {
        return;
    };
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-runtime-shutdown".into()),
            "desktop runtime shutdown",
        );
    }
    match tokio::time::timeout(shutdown_deadline, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            let _ = session.shutdown().await;
        }
        Ok(Err(_)) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: local_runtime_error(
                        "runtime_task_panicked",
                        "A desktop runtime task stopped unexpectedly.",
                    ),
                })
                .await;
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: DesktopRuntimeError {
                        code: "shutdown_deadline_exceeded".into(),
                        message: format!(
                            "prompt operation {} did not stop within {} seconds",
                            operation_id.as_deref().unwrap_or("<starting>"),
                            shutdown_deadline.as_secs_f64()
                        ),
                    },
                })
                .await;
        }
    }
    let _ = active.connection.detach();
}

trait DesktopRuntimeErrorSource {
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
            DesktopBridgeError::Session { .. } => {
                local_runtime_error("session", "The desktop session operation failed.")
            }
            DesktopBridgeError::Busy { .. } => {
                local_runtime_error("busy", "The desktop runtime is busy.")
            }
            DesktopBridgeError::ExternalEditor => local_runtime_error(
                "external_editor_unavailable",
                "The configured external editor could not be started.",
            ),
        }
    }
}

fn runtime_error(error: &impl DesktopRuntimeErrorSource) -> DesktopRuntimeError {
    error.project_runtime_error()
}

fn local_runtime_error(code: &str, message: &str) -> DesktopRuntimeError {
    DesktopRuntimeError {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn validate_session_id(session_id: &str) -> Result<(), DesktopCommandAdmissionError> {
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

fn bounded_utf8_prefix(value: &str, max_bytes: usize) -> String {
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

fn validate_prompt(prompt: &str) -> Result<(), DesktopCommandAdmissionError> {
    if prompt.trim().is_empty() {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: "prompt must not be empty".into(),
        });
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(DesktopCommandAdmissionError::InvalidPrompt {
            message: format!("prompt exceeds {MAX_PROMPT_BYTES} bytes"),
        });
    }
    Ok(())
}

fn validate_control_text(text: &str) -> Result<(), DesktopCommandAdmissionError> {
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

fn validate_file_review_request(
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

fn validate_authorization_identity(
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

fn validate_recovery_identity(
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

fn validate_selection_id(selection: &str, id: &str) -> Result<(), DesktopCommandAdmissionError> {
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    use coding_agent::api::authorization::{
        ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
        ToolAuthorizationScope,
    };
    use coding_agent::api::client::{
        CodingAgentClientBootstrap, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    };
    use coding_agent::api::error::{CodingAgentErrorCategory, CodingAgentErrorContext};
    use coding_agent::api::view::CodingAgentSessionTranscriptItem;

    use crate::conversation::{MAX_TRANSCRIPT_BLOCKS, MAX_TRANSCRIPT_BYTES};
    use crate::projection::{
        ContextDirtyFlags, DesktopMessageStatus, DesktopProjection, DesktopProjectionApply,
        DesktopProjectionLifecycle, DesktopToolStatus, MAX_AUTHORIZATION_TEXT_BYTES,
        MAX_DESKTOP_MESSAGE_OVERLAYS,
    };

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ProcessEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl ProcessEnvGuard {
        fn isolated(evo_dir: &std::path::Path) -> Self {
            const NAMES: &[&str] = &[
                "EVO_DIR",
                "ANTHROPIC_API_KEY",
                "CLAUDE_API_KEY",
                "ANTHROPIC_KEY",
            ];
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = NAMES
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            unsafe {
                std::env::set_var("EVO_DIR", evo_dir);
                for name in &NAMES[1..] {
                    std::env::remove_var(name);
                }
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for ProcessEnvGuard {
        fn drop(&mut self) {
            for (name, previous) in self.saved.iter().rev() {
                unsafe {
                    match previous {
                        Some(previous) => std::env::set_var(name, previous),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn isolated_options(
        temp: &tempfile::TempDir,
    ) -> (ProcessEnvGuard, CodingAgentEmbeddingOptions) {
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        let sessions = temp.path().join("sessions");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let env = ProcessEnvGuard::isolated(&global);
        let options = CodingAgentEmbeddingOptions::new(&project)
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
        (env, options)
    }

    fn start_runtime(
        options: CodingAgentEmbeddingOptions,
    ) -> (DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot) {
        DesktopRuntimeBridge::spawn(options)
            .unwrap()
            .wait_blocking()
            .unwrap()
    }

    #[test]
    fn desktop_runtime_enables_tcp_io() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(address).unwrap();
        let (_server, _) = listener.accept().unwrap();
        client.set_nonblocking(true).unwrap();

        build_desktop_runtime().unwrap().block_on(async move {
            let stream = tokio::net::TcpStream::from_std(client).unwrap();
            stream.writable().await.unwrap();
        });
    }

    #[tokio::test]
    async fn bootstrap_can_be_polled_without_waiting_on_runtime_initialization() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let mut bootstrap = DesktopRuntimeBridge::spawn(options).unwrap();

        let (bridge, snapshot) = loop {
            if let Some(ready) = bootstrap.try_ready().unwrap() {
                break ready;
            }
            tokio::task::yield_now().await;
        };
        assert!(!snapshot.session.session.session_id.is_empty());
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn runtime_owns_context_and_switches_sessions_over_bounded_queues() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, initial) = start_runtime(options);
        assert_eq!(
            initial.transcript.session_id,
            initial.session.session.session_id
        );
        let initial_session_id = initial.session.session.session_id.clone();

        bridge.try_create_session(1).unwrap();
        let DesktopRuntimeUpdate::SessionChanged {
            command_id,
            snapshot,
        } = bridge.next_update().await.unwrap()
        else {
            panic!("create session should publish a replacement snapshot");
        };
        assert_eq!(command_id, 1);
        assert_ne!(snapshot.session.session.session_id, initial_session_id);

        bridge.try_open_session(2, &initial_session_id).unwrap();
        let DesktopRuntimeUpdate::SessionChanged {
            command_id,
            snapshot,
        } = bridge.next_update().await.unwrap()
        else {
            panic!("open session should publish a replacement snapshot");
        };
        assert_eq!(command_id, 2);
        assert_eq!(snapshot.session.session.session_id, initial_session_id);

        bridge.try_open_session(3, "missing-session").unwrap();
        let DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command,
            ..
        } = bridge.next_update().await.unwrap()
        else {
            panic!("missing session should be rejected");
        };
        assert_eq!(command_id, 3);
        assert_eq!(command, DesktopRuntimeCommandKind::OpenSession);

        bridge.try_reload(4).unwrap();
        let DesktopRuntimeUpdate::Reloaded {
            command_id,
            metadata,
        } = bridge.next_update().await.unwrap()
        else {
            panic!("reload should publish the retained current session");
        };
        assert_eq!(command_id, 4);
        assert_eq!(metadata.session.session.session_id, initial_session_id);

        bridge.try_resync(5).unwrap();
        let DesktopRuntimeUpdate::Resynced {
            command_id,
            replacement,
        } = bridge.next_update().await.unwrap()
        else {
            panic!("idle resync should publish a consistent runtime snapshot");
        };
        assert_eq!(command_id, 5);
        let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
            panic!("idle resync must hydrate durable state");
        };
        assert_eq!(snapshot.session.session.session_id, initial_session_id);

        bridge.try_list_sessions(6).unwrap();
        let DesktopRuntimeUpdate::SessionsListed {
            command_id,
            sessions,
            omitted,
        } = bridge.next_update().await.unwrap()
        else {
            panic!("session catalog should use a typed bounded update");
        };
        assert_eq!(command_id, 6);
        assert_eq!(omitted, 0);
        assert!(sessions.len() >= 2);
        assert!(sessions.len() <= MAX_DESKTOP_SESSION_CATALOG);
        assert!(
            sessions
                .iter()
                .any(|session| session.session_id == initial_session_id)
        );

        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changed_file_review_command_is_typed_and_preserves_product_error_codes() {
        use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, _) = start_runtime(options);
        let (commands, mut events, shutdown) = bridge.into_parts();
        let request = CodingAgentFileReviewRequest::new(
            CodingAgentFileChangeIdentity {
                operation_id: "operation-review".into(),
                tool_call_id: Some("call-review".into()),
                path: "src/lib.rs".into(),
            },
            CodingAgentFileRevision::new(7),
        );

        commands.try_review_changed_file(41, &request).unwrap();
        let update = events.next_update().await.unwrap();
        assert!(matches!(
            update,
            DesktopRuntimeUpdate::CommandRejected {
                command_id: 41,
                command: DesktopRuntimeCommandKind::ReviewChangedFile,
                code,
                ..
            } if code == "file_review_change_unauthorized"
        ));

        let mut oversized = request;
        oversized.change.path = "x".repeat(MAX_FILE_REVIEW_PATH_BYTES + 1);
        assert!(matches!(
            commands.try_review_changed_file(42, &oversized),
            Err(DesktopCommandAdmissionError::InvalidFileReview { .. })
        ));

        drop(commands);
        shutdown.shutdown(&mut events).await.unwrap();
    }

    #[tokio::test]
    async fn failed_reload_retains_the_previous_runtime_context() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, _) = isolated_options(&temp);
        let options = CodingAgentEmbeddingOptions::new(temp.path().join("project"))
            .with_session_dir(temp.path().join("sessions"));
        let (mut bridge, initial) = start_runtime(options);
        std::fs::write(
            temp.path().join("global").join("settings.toml"),
            "default_model = \"missing-desktop-reload-model\"\n",
        )
        .unwrap();

        bridge.try_reload(6).unwrap();
        let reload_update = bridge.next_update().await;
        assert!(
            matches!(
                &reload_update,
                Some(DesktopRuntimeUpdate::CommandRejected {
                    command_id: 6,
                    command: DesktopRuntimeCommandKind::Reload,
                    code,
                    ..
                }) if code == "config"
            ),
            "unexpected reload result: {reload_update:?}"
        );

        bridge.try_resync(7).unwrap();
        let Some(DesktopRuntimeUpdate::Resynced {
            command_id: 7,
            replacement,
        }) = bridge.next_update().await
        else {
            panic!("resync after a failed reload must return the retained context");
        };
        let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
            panic!("idle resync must hydrate durable state");
        };
        assert_eq!(snapshot.project, initial.project);
        assert_eq!(
            snapshot.session.session.session_id,
            initial.session.session.session_id
        );
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn idle_model_and_session_profile_selection_are_typed_and_transactional() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, initial) = start_runtime(options);
        let session_id = initial.session.session.session_id.clone();
        let mut projection = DesktopProjection::new(initial).unwrap();
        let conversation = projection.conversation().clone();

        bridge.try_select_model(8, "claude-haiku-4-5").unwrap();
        let update = bridge.next_update().await.unwrap();
        let DesktopRuntimeUpdate::SelectionChanged {
            command_id: 8,
            selection: DesktopRuntimeSelectionKind::Model,
            metadata,
        } = &update
        else {
            panic!("idle model selection must return a typed replacement snapshot");
        };
        assert_eq!(metadata.project.selected_model_id, "claude-haiku-4-5");
        assert_eq!(metadata.session.session.session_id, session_id);
        assert!(projection.apply(update).is_replaced());
        assert_eq!(projection.conversation(), &conversation);

        bridge.try_select_session_profile(9, "review").unwrap();
        let update = bridge.next_update().await.unwrap();
        let DesktopRuntimeUpdate::SelectionChanged {
            command_id: 9,
            selection: DesktopRuntimeSelectionKind::SessionProfile,
            metadata,
        } = &update
        else {
            panic!("idle profile selection must return a typed replacement snapshot");
        };
        assert_eq!(
            metadata.session.session.default_agent_profile_id.as_str(),
            "review"
        );
        assert_eq!(metadata.project.selected_model_id, "claude-haiku-4-5");
        assert!(projection.apply(update).is_replaced());
        assert_eq!(projection.conversation(), &conversation);

        bridge
            .try_select_model(10, "missing-desktop-model")
            .unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 10,
                command: DesktopRuntimeCommandKind::SelectModel,
                ..
            })
        ));
        bridge
            .try_select_session_profile(11, "missing-profile")
            .unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 11,
                command: DesktopRuntimeCommandKind::SelectSessionProfile,
                ..
            })
        ));

        bridge.try_resync(12).unwrap();
        let Some(DesktopRuntimeUpdate::Resynced { replacement, .. }) = bridge.next_update().await
        else {
            panic!("resync must expose the last successful selector state");
        };
        let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
            panic!("idle resync must hydrate durable state");
        };
        assert_eq!(snapshot.project.selected_model_id, "claude-haiku-4-5");
        assert_eq!(
            snapshot.session.session.default_agent_profile_id.as_str(),
            "review"
        );
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn ten_mib_transcript_stays_single_hydration_across_metadata_commands() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, mut initial) = start_runtime(options);
        let payload = "x".repeat(1_280);
        initial.transcript.items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
            })
            .collect();
        let fixture_bytes = initial
            .transcript
            .items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);
        let metadata = DesktopRuntimeMetadataSnapshot {
            project: initial.project.clone(),
            session: initial.session.clone(),
        };
        let recovery = DesktopRuntimeRecoverySnapshot {
            project: initial.project.clone(),
            session: initial.session.clone(),
            pending_recoveries: Vec::new(),
        };
        let mut projection = DesktopProjection::new(initial).unwrap();
        let initial_counters = projection.counters();
        assert_eq!(initial_counters.full_transcript_hydrations, 1);
        assert_eq!(
            initial_counters.transcript_items_hydrated,
            MAX_TRANSCRIPT_BLOCKS as u64
        );
        assert_eq!(
            initial_counters.conversation_blocks_allocated,
            MAX_TRANSCRIPT_BLOCKS as u64
        );
        assert!(projection.conversation().retained_bytes() <= MAX_TRANSCRIPT_BYTES);

        for command_id in 100..164 {
            let update = match command_id % 4 {
                0 => DesktopRuntimeUpdate::Reloaded {
                    command_id,
                    metadata: metadata.clone(),
                },
                1 => DesktopRuntimeUpdate::SelectionChanged {
                    command_id,
                    selection: DesktopRuntimeSelectionKind::Model,
                    metadata: metadata.clone(),
                },
                2 => DesktopRuntimeUpdate::SelectionChanged {
                    command_id,
                    selection: DesktopRuntimeSelectionKind::SessionProfile,
                    metadata: metadata.clone(),
                },
                _ => DesktopRuntimeUpdate::PromptStarted {
                    command_id,
                    operation_id: format!("metadata-operation-{command_id}"),
                    metadata: metadata.clone(),
                },
            };
            assert!(projection.apply(update).is_replaced());
        }
        for command_id in 164..180 {
            assert!(
                projection
                    .apply(DesktopRuntimeUpdate::RecoveryChanged {
                        command_id,
                        action: DesktopRecoveryAction::Retry,
                        recovery_id: format!("recovery-{command_id}"),
                        recovery: recovery.clone(),
                    })
                    .is_replaced()
            );
        }

        let counters = projection.counters();
        assert_eq!(counters.full_transcript_hydrations, 1);
        assert_eq!(
            counters.transcript_items_hydrated,
            MAX_TRANSCRIPT_BLOCKS as u64
        );
        assert_eq!(
            counters.conversation_blocks_allocated,
            MAX_TRANSCRIPT_BLOCKS as u64
        );
        assert_eq!(counters.metadata_replacements, 64);
        assert_eq!(counters.recovery_replacements, 16);
        assert_eq!(
            projection.conversation().blocks().len(),
            MAX_TRANSCRIPT_BLOCKS
        );
        assert!(
            projection
                .conversation()
                .blocks()
                .front()
                .unwrap()
                .text
                .starts_with("0:")
        );
        assert!(
            projection
                .conversation()
                .blocks()
                .back()
                .unwrap()
                .text
                .starts_with("9999:")
        );
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn prompt_submission_forwards_product_events_and_returns_the_session_owner() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, initial) = start_runtime(options);
        let session_id = initial.session.session.session_id;

        bridge
            .try_submit_prompt(10, "offline desktop prompt", None)
            .unwrap();
        let mut started_operation_id = None;
        let mut saw_product_event = false;
        let mut last_product_event_sequence = None;
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match bridge.next_update().await.unwrap() {
                    DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                        assert_eq!(command_id, 10);
                    }
                    DesktopRuntimeUpdate::PromptStarted {
                        command_id,
                        operation_id,
                        ..
                    } => {
                        assert_eq!(command_id, 10);
                        started_operation_id = Some(operation_id);
                    }
                    DesktopRuntimeUpdate::ProductEvent { event } => {
                        saw_product_event = true;
                        if let Some(previous) = last_product_event_sequence {
                            assert!(
                                event.sequence() > previous,
                                "desktop bridge reordered product event {} after {previous}",
                                event.sequence()
                            );
                        }
                        last_product_event_sequence = Some(event.sequence());
                        if let Some(started) = started_operation_id.as_deref()
                            && let Some(event_operation_id) = event.operation_id()
                        {
                            assert_eq!(event_operation_id, started);
                        }
                    }
                    DesktopRuntimeUpdate::PromptFinished {
                        command_id,
                        operation_id,
                        snapshot,
                        ..
                    } => {
                        assert_eq!(command_id, 10);
                        assert_eq!(Some(operation_id.as_str()), started_operation_id.as_deref());
                        assert_eq!(snapshot.session.session.session_id, session_id);
                        let transcript = &snapshot.transcript;
                        assert_eq!(transcript.session_id, session_id);
                        assert!(transcript.items.iter().any(|item| matches!(
                            item,
                            coding_agent::api::view::CodingAgentSessionTranscriptItem::User {
                                text
                            } if text == "offline desktop prompt"
                        )));
                        break;
                    }
                    DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                    update => panic!("unexpected prompt update: {update:?}"),
                }
            }
        })
        .await;
        assert!(finished.is_ok(), "offline prompt did not finish promptly");
        assert!(saw_product_event);

        bridge.try_create_session(11).unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::SessionChanged { command_id: 11, .. })
        ));
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn desktop_projection_rejects_gaps_and_association_mismatches_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, initial) = start_runtime(options);
        let mut wrong_transcript = initial.clone();
        wrong_transcript.transcript.session_id = "wrong-session".into();
        assert_eq!(
            DesktopProjection::new(wrong_transcript).unwrap_err().code,
            "transcript_session_mismatch"
        );
        let mut projection = DesktopProjection::new(initial).unwrap();
        bridge
            .try_submit_prompt(40, "projection cursor fixture", None)
            .unwrap();

        let mut exercised_strict_reducer = false;
        let mut requested_active_resync = false;
        let mut saw_active_resync = false;
        let mut saw_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let update = bridge.next_update().await.unwrap();
                if matches!(update, DesktopRuntimeUpdate::PromptStarted { .. })
                    && !requested_active_resync
                {
                    bridge.try_resync(41).unwrap();
                    requested_active_resync = true;
                }
                if let DesktopRuntimeUpdate::Resynced { command_id: 41, .. } = &update {
                    saw_active_resync = true;
                }
                if let DesktopRuntimeUpdate::ProductEvent { event } = &update
                    && !exercised_strict_reducer
                {
                    let mut baseline = projection.clone();
                    let expected = baseline.cursor().last_event_sequence + 1;
                    let submitted_operation = baseline
                        .snapshot()
                        .submitted_operation
                        .as_ref()
                        .map(|operation| operation.operation_id.clone());

                    let valid = rewritten_event(
                        event,
                        expected,
                        baseline.cursor().stream_id.as_str(),
                        Some(baseline.snapshot().session.session_id.as_str()),
                        submitted_operation.as_deref(),
                    );
                    assert!(
                        baseline
                            .apply(DesktopRuntimeUpdate::ProductEvent {
                                event: valid.clone(),
                            })
                            .is_applied()
                    );
                    assert_eq!(
                        baseline.apply(DesktopRuntimeUpdate::ProductEvent { event: valid }),
                        DesktopProjectionApply::IgnoredDuplicate
                    );

                    let mut gap_projection = projection.clone();
                    let original_cursor = gap_projection.cursor().clone();
                    let gap = rewritten_event(
                        event,
                        expected + 1,
                        gap_projection.cursor().stream_id.as_str(),
                        Some(gap_projection.snapshot().session.session_id.as_str()),
                        submitted_operation.as_deref(),
                    );
                    assert_eq!(
                        gap_projection.apply(DesktopRuntimeUpdate::ProductEvent { event: gap }),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(gap_projection.cursor(), &original_cursor);
                    assert_eq!(
                        gap_projection.lifecycle(),
                        DesktopProjectionLifecycle::NeedsResync
                    );
                    assert!(
                        gap_projection
                            .apply(DesktopRuntimeUpdate::ResyncRequired {
                                reason: DesktopRuntimeError {
                                    code: "test_resync".into(),
                                    message: "replace after an injected cursor gap".into(),
                                },
                                snapshot: projection.snapshot().clone(),
                            })
                            .is_replaced()
                    );
                    assert_eq!(
                        gap_projection.lifecycle(),
                        DesktopProjectionLifecycle::Running
                    );
                    assert!(gap_projection.recent_events().is_empty());

                    let mut wrong_session = projection.clone();
                    let mismatched = rewritten_event(
                        event,
                        expected,
                        wrong_session.cursor().stream_id.as_str(),
                        Some("another-session"),
                        submitted_operation.as_deref(),
                    );
                    assert_eq!(
                        wrong_session
                            .apply(DesktopRuntimeUpdate::ProductEvent { event: mismatched }),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(
                        wrong_session.issues().back().unwrap().code,
                        "product_event_session_mismatch"
                    );

                    let mut wrong_stream = projection.clone();
                    let mismatched = rewritten_event(
                        event,
                        expected,
                        "another-stream",
                        Some(wrong_stream.snapshot().session.session_id.as_str()),
                        submitted_operation.as_deref(),
                    );
                    assert_eq!(
                        wrong_stream
                            .apply(DesktopRuntimeUpdate::ProductEvent { event: mismatched }),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(
                        wrong_stream.issues().back().unwrap().code,
                        "product_event_stream_mismatch"
                    );

                    let mut wrong_generation = projection.clone();
                    let mut value = serde_json::to_value(rewritten_event(
                        event,
                        expected,
                        wrong_generation.cursor().stream_id.as_str(),
                        Some(wrong_generation.snapshot().session.session_id.as_str()),
                        submitted_operation.as_deref(),
                    ))
                    .unwrap();
                    value["capability_generation"] = serde_json::json!(
                        wrong_generation
                            .cursor()
                            .capability_generation
                            .saturating_add(2)
                    );
                    let mismatched = serde_json::from_value(value).unwrap();
                    assert_eq!(
                        wrong_generation
                            .apply(DesktopRuntimeUpdate::ProductEvent { event: mismatched }),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(
                        wrong_generation.issues().back().unwrap().code,
                        "product_event_capability_generation_mismatch"
                    );

                    if submitted_operation.is_some() {
                        let mut wrong_operation = projection.clone();
                        let mismatched = rewritten_event(
                            event,
                            expected,
                            wrong_operation.cursor().stream_id.as_str(),
                            Some(wrong_operation.snapshot().session.session_id.as_str()),
                            Some("unrelated-operation"),
                        );
                        assert_eq!(
                            wrong_operation
                                .apply(DesktopRuntimeUpdate::ProductEvent { event: mismatched }),
                            DesktopProjectionApply::NeedsResync
                        );
                        assert_eq!(
                            wrong_operation.issues().back().unwrap().code,
                            "product_event_operation_mismatch"
                        );
                    }
                    assert_bounded_streaming_overlays(
                        &projection,
                        event,
                        submitted_operation.as_deref(),
                    );
                    exercised_strict_reducer = true;
                }

                saw_finished |= matches!(update, DesktopRuntimeUpdate::PromptFinished { .. });
                let outcome = projection.apply(update);
                assert_ne!(
                    outcome,
                    DesktopProjectionApply::NeedsResync,
                    "real runtime updates must satisfy the desktop projection contract: {:?}",
                    projection.issues().back()
                );
                if saw_finished && saw_active_resync {
                    break;
                }
            }
        })
        .await
        .expect("projection fixture prompt must finish");
        assert!(exercised_strict_reducer);
        assert!(saw_active_resync);
        assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Running);
        assert!(
            projection
                .conversation()
                .blocks()
                .iter()
                .any(|block| block.text == "projection cursor fixture")
        );
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shared_cross_adapter_fixture_matches_desktop_product_state_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let transcript = initial.transcript.clone();
        let mut shared = CodingAgentClientProjection::from_bootstrap(CodingAgentClientBootstrap {
            snapshot: initial.session.clone(),
            transcript,
            pending_recoveries: initial.pending_recoveries.clone(),
        })
        .unwrap();
        let mut desktop = DesktopProjection::new(initial).unwrap();
        let base_sequence = desktop.cursor().last_event_sequence;
        let stream_id = desktop.cursor().stream_id.clone();
        let session_id = desktop.snapshot().session.session_id.clone();

        for fixture in cross_adapter_fixture_events() {
            let event = rewritten_event(
                &fixture,
                base_sequence + fixture.sequence(),
                &stream_id,
                Some(&session_id),
                fixture.operation_id(),
            );
            assert!(matches!(
                shared.apply(&event),
                CodingAgentClientProjectionApply::Applied(_)
            ));
            let terminal = event.terminal_operation().is_some();
            let outcome = desktop.apply(DesktopRuntimeUpdate::ProductEvent { event });
            assert!(outcome.is_applied());
            assert_eq!(outcome.delta().unwrap().terminal, terminal);
        }

        assert_eq!(desktop.product_for_tests(), &shared);
        assert_eq!(
            desktop
                .messages()
                .front()
                .map(|message| message.text.as_str()),
            Some("hello world")
        );
        assert_eq!(
            desktop.tools().front().map(|tool| tool.detail.as_str()),
            Some("read complete")
        );
        assert_eq!(
            desktop.snapshot().context.delegations[0].status,
            "completed"
        );
        assert_eq!(
            desktop.snapshot().session.default_agent_profile_id.as_str(),
            "reviewer"
        );
        bridge.shutdown().await.unwrap();
    }

    fn rewritten_event(
        event: &CodingAgentProductEvent,
        sequence: u64,
        stream_id: &str,
        session_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> CodingAgentProductEvent {
        let mut value = serde_json::to_value(event).unwrap();
        value["sequence"] = serde_json::json!(sequence);
        value["stream_id"] = serde_json::json!(stream_id);
        value["session_id"] = session_id.map_or(serde_json::Value::Null, |session_id| {
            serde_json::json!(session_id)
        });
        value["operation_id"] = operation_id.map_or(serde_json::Value::Null, |operation_id| {
            serde_json::json!(operation_id)
        });
        value["parent_operation_id"] = serde_json::Value::Null;
        value["root_operation_id"] = serde_json::Value::Null;
        serde_json::from_value(value).unwrap()
    }

    fn cross_adapter_fixture_events() -> Vec<CodingAgentProductEvent> {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
        )))
        .expect("the shared client-projection fixture must deserialize")
    }

    fn rewritten_event_kind(
        event: &CodingAgentProductEvent,
        sequence: u64,
        stream_id: &str,
        session_id: &str,
        operation_id: &str,
        kind: serde_json::Value,
    ) -> CodingAgentProductEvent {
        let rewritten = rewritten_event(
            event,
            sequence,
            stream_id,
            Some(session_id),
            Some(operation_id),
        );
        let mut value = serde_json::to_value(rewritten).unwrap();
        value["event"] = kind;
        value["terminal_status"] = serde_json::Value::Null;
        value["terminal_operation"] = serde_json::Value::Null;
        serde_json::from_value(value).unwrap()
    }

    fn assert_bounded_streaming_overlays(
        projection: &DesktopProjection,
        base_event: &CodingAgentProductEvent,
        submitted_operation: Option<&str>,
    ) {
        let Some(operation_id) = submitted_operation else {
            return;
        };
        let mut overlays = projection.clone();
        let stream_id = overlays.cursor().stream_id.clone();
        let session_id = overlays.snapshot().session.session_id.clone();
        let initial_usage_input = overlays.snapshot().context.usage.input;
        let initial_usage_output = overlays.snapshot().context.usage.output;
        let initial_view_rebuilds = overlays.counters().product_view_rebuilds;
        let mut sequence = overlays.cursor().last_event_sequence;

        sequence += 1;
        let started = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "started",
                    "operation_id": operation_id,
                    "turn_id": "turn-overlay",
                    "message_id": "message-overlay"
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent { event: started });
        assert!(outcome.is_applied());
        let delta = outcome.delta().unwrap();
        assert!(delta.cursor);
        assert!(delta.conversation);
        assert!(!delta.tools);
        assert!(!delta.context.contains(ContextDirtyFlags::USAGE));

        sequence += 1;
        let delta = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "delta",
                    "operation_id": operation_id,
                    "turn_id": "turn-overlay",
                    "message_id": "message-overlay",
                    "text": "streaming text"
                }
            }),
        );
        assert!(
            overlays
                .apply(DesktopRuntimeUpdate::ProductEvent { event: delta })
                .is_applied()
        );

        sequence += 1;
        let completed = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "completed",
                    "operation_id": operation_id,
                    "turn_id": "turn-overlay",
                    "message_id": "message-overlay",
                    "final_text": "final text",
                    "images": [],
                    "usage": {
                        "input": 1,
                        "output": 2,
                        "cache_read": 0,
                        "cache_write": 0,
                        "total_tokens": 3,
                        "cost_known": false,
                        "input_cost": 0.0,
                        "output_cost": 0.0,
                        "cache_read_cost": 0.0,
                        "cache_write_cost": 0.0
                    }
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent { event: completed });
        assert!(outcome.is_applied());
        let delta = outcome.delta().unwrap();
        assert!(delta.conversation);
        assert!(delta.context.contains(ContextDirtyFlags::USAGE));
        let message = overlays.messages().back().unwrap();
        assert_eq!(message.text, "final text");
        assert_eq!(message.status, DesktopMessageStatus::Completed);
        assert_eq!(
            overlays.snapshot().context.usage.input,
            initial_usage_input + 1
        );
        assert_eq!(
            overlays.snapshot().context.usage.output,
            initial_usage_output + 2
        );

        for index in 0..=MAX_DESKTOP_MESSAGE_OVERLAYS {
            sequence += 1;
            let completed = rewritten_event_kind(
                base_event,
                sequence,
                &stream_id,
                &session_id,
                operation_id,
                serde_json::json!({
                    "family": "message",
                    "payload": {
                        "kind": "completed",
                        "operation_id": operation_id,
                        "turn_id": format!("turn-{index}"),
                        "message_id": format!("message-{index}"),
                        "final_text": "bounded",
                        "images": [],
                        "usage": {
                            "input": 0,
                            "output": 0,
                            "cache_read": 0,
                            "cache_write": 0,
                            "total_tokens": 0,
                            "cost_known": false,
                            "input_cost": 0.0,
                            "output_cost": 0.0,
                            "cache_read_cost": 0.0,
                            "cache_write_cost": 0.0
                        }
                    }
                }),
            );
            assert!(
                overlays
                    .apply(DesktopRuntimeUpdate::ProductEvent { event: completed })
                    .is_applied()
            );
        }
        assert_eq!(overlays.messages().len(), MAX_DESKTOP_MESSAGE_OVERLAYS);

        sequence += 1;
        let tool_started = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "tool",
                "payload": {
                    "kind": "started",
                    "operation_id": operation_id,
                    "turn_id": "turn-tool",
                    "tool_call_id": "tool-overlay",
                    "name": "edit",
                    "arguments_json": "{\"path\":\"README.md\"}"
                }
            }),
        );
        assert!(
            overlays
                .apply(DesktopRuntimeUpdate::ProductEvent {
                    event: tool_started,
                })
                .is_applied()
        );
        sequence += 1;
        let tool_completed = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "tool",
                "payload": {
                    "kind": "completed",
                    "operation_id": operation_id,
                    "turn_id": "turn-tool",
                    "tool_call_id": "tool-overlay",
                    "name": "edit",
                    "summary": "edited README.md"
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent {
            event: tool_completed,
        });
        assert!(outcome.is_applied());
        let delta = outcome.delta().unwrap();
        assert!(delta.tools);
        assert!(delta.context.contains(ContextDirtyFlags::CHANGES));
        assert!(!delta.conversation);
        assert_eq!(
            overlays.tools().back().unwrap().status,
            DesktopToolStatus::Completed
        );
        assert_eq!(
            overlays.snapshot().context.changes.first().unwrap().path,
            "README.md"
        );

        sequence += 1;
        let delegation = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "delegation",
                "payload": {
                    "kind": "started",
                    "context": {
                        "operation_id": operation_id,
                        "turn_id": "turn-delegation",
                        "tool_call_id": "delegation-overlay",
                        "requesting_profile_id": "default",
                        "target_kind": "agent",
                        "target_id": "reviewer",
                        "task": "review projection"
                    },
                    "child_operation_id": "child-overlay"
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent { event: delegation });
        assert!(outcome.is_applied());
        let delta = outcome.delta().unwrap();
        assert!(delta.context.contains(ContextDirtyFlags::DELEGATIONS));
        assert!(!delta.conversation);
        assert!(!delta.tools);
        assert_eq!(
            overlays
                .snapshot()
                .context
                .delegations
                .first()
                .unwrap()
                .status,
            "running"
        );

        sequence += 1;
        let recovery = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "workflow",
                "payload": {
                    "kind": "operation_recovery_pending",
                    "operation_id": operation_id,
                    "recovery_id": "recovery-overlay",
                    "reason": "injected recovery",
                    "record_version": 1,
                    "descriptor_revision": 1,
                    "capability_generation": null,
                    "attempt_count": 0,
                    "last_attempt_at": null,
                    "next_attempt_at": null
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent { event: recovery });
        assert!(outcome.is_applied());
        assert!(outcome.delta().unwrap().recoveries);
        assert_eq!(
            overlays.recoveries().front().unwrap().status,
            crate::projection::DesktopRecoveryStatus::Pending
        );

        sequence += 1;
        let diagnostic = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "diagnostic",
                "payload": {
                    "kind": "diagnostic",
                    "diagnostic": {
                        "severity": "warning",
                        "code": "projection_diagnostic",
                        "summary": "projection diagnostic",
                        "origin": "runtime",
                        "operation_id": operation_id
                    }
                }
            }),
        );
        let outcome = overlays.apply(DesktopRuntimeUpdate::ProductEvent { event: diagnostic });
        assert!(outcome.is_applied());
        assert!(outcome.delta().unwrap().diagnostics);
        assert_eq!(
            overlays.diagnostics().back().unwrap().message,
            "projection diagnostic"
        );
        let incremental_counters = overlays.counters();
        assert_eq!(
            incremental_counters.product_view_rebuilds, initial_view_rebuilds,
            "product events must not rebuild every compatibility view"
        );
        assert!(incremental_counters.incremental_message_updates > 1);
        assert_eq!(incremental_counters.incremental_tool_updates, 2);
        assert_eq!(incremental_counters.incremental_recovery_updates, 1);
        assert_eq!(incremental_counters.incremental_diagnostic_updates, 1);

        let mut fresh = overlays.snapshot().clone();
        fresh.cursor = overlays.cursor().clone();
        assert!(
            overlays
                .apply(DesktopRuntimeUpdate::ResyncRequired {
                    reason: DesktopRuntimeError {
                        code: "overlay_resync".into(),
                        message: "discard incomplete live overlays".into(),
                    },
                    snapshot: fresh,
                })
                .is_replaced()
        );
        assert!(overlays.messages().is_empty());
        assert!(overlays.tools().is_empty());
        assert_eq!(
            overlays.counters().product_view_rebuilds,
            initial_view_rebuilds + 1
        );
        assert_eq!(
            overlays
                .recoveries()
                .front()
                .map(|recovery| recovery.recovery_id.as_str()),
            Some("recovery-overlay")
        );
        assert!(!overlays.recoveries().front().unwrap().authoritative);
        assert!(overlays.diagnostics().is_empty());
    }

    #[tokio::test]
    async fn command_queue_full_and_closed_are_typed_without_runtime_timing() {
        let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (shutdown, _shutdown_rx) = watch::channel(false);
        let (_priority_updates_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (_data_updates_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let bridge = DesktopRuntimeBridge {
            shutdown: DesktopRuntimeShutdownGuard {
                shutdown,
                runtime_thread: None,
            },
            commands: Some(commands),
            events: DesktopRuntimeEventStream {
                priority_updates,
                data_updates,
                pending_priority_update: None,
                pending_data_update: None,
            },
        };

        for command_id in 0..DESKTOP_COMMAND_QUEUE_CAPACITY as u64 {
            bridge.try_reload(command_id).unwrap();
        }
        assert_eq!(
            bridge.try_reload(u64::MAX),
            Err(DesktopCommandAdmissionError::QueueFull)
        );
        drop(_command_rx);
        assert_eq!(
            bridge.try_reload(u64::MAX),
            Err(DesktopCommandAdmissionError::RuntimeClosed)
        );
    }

    #[tokio::test]
    async fn streaming_batch_waits_only_for_data_and_flushes_on_priority_delivery() {
        let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (shutdown, _shutdown_rx) = watch::channel(false);
        let (priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let mut bridge = DesktopRuntimeBridge {
            shutdown: DesktopRuntimeShutdownGuard {
                shutdown,
                runtime_thread: None,
            },
            commands: Some(commands),
            events: DesktopRuntimeEventStream {
                priority_updates,
                data_updates,
                pending_priority_update: None,
                pending_data_update: None,
            },
        };
        let fixture = cross_adapter_fixture_events();
        let data = fixture
            .iter()
            .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
            .cloned()
            .expect("fixture must contain a coalescible data event");
        let priority = fixture
            .iter()
            .find(|event| event.delivery_class() != CodingAgentProductEventDeliveryClass::Data)
            .cloned()
            .expect("fixture must contain an immediate event");

        data_tx
            .send(DesktopRuntimeUpdate::ProductEvent {
                event: data.clone(),
            })
            .await
            .unwrap();
        let priority_task = tokio::spawn(async move {
            tokio::task::yield_now().await;
            priority_tx
                .send(DesktopRuntimeUpdate::ProductEvent {
                    event: priority.clone(),
                })
                .await
                .unwrap();
            priority
        });
        let batch = bridge.next_update_batch().await.unwrap();
        let priority = priority_task.await.unwrap();

        assert_eq!(batch.len(), 2);
        assert!(matches!(
            &batch[0],
            DesktopRuntimeUpdate::ProductEvent { event } if event == &data
        ));
        assert!(matches!(
            &batch[1],
            DesktopRuntimeUpdate::ProductEvent { event } if event == &priority
        ));
    }

    #[test]
    fn streaming_batch_timer_does_not_require_a_tokio_reactor() {
        let (_priority_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let data = cross_adapter_fixture_events()
            .into_iter()
            .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
            .expect("fixture must contain a coalescible data event");
        data_tx
            .try_send(DesktopRuntimeUpdate::ProductEvent { event: data })
            .unwrap();
        let mut events = DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        };

        let mut future = std::pin::pin!(events.next_update_batch());
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let batch = loop {
            match std::future::Future::poll(future.as_mut(), &mut context) {
                std::task::Poll::Ready(batch) => break batch.expect("data update should be ready"),
                std::task::Poll::Pending => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "executor-neutral coalescing timer did not complete"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };

        assert_eq!(batch.len(), 1);
    }

    #[tokio::test]
    async fn data_queue_overflow_emits_a_priority_resync_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let (priority_updates, mut priority_rx) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_updates, _data_rx) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        for command_id in 0..DESKTOP_UPDATE_QUEUE_CAPACITY as u64 {
            data_updates
                .try_send(DesktopRuntimeUpdate::PromptAccepted { command_id })
                .unwrap();
        }

        assert!(
            publish_data_update(
                DesktopRuntimeUpdate::PromptAccepted {
                    command_id: u64::MAX,
                },
                || Ok::<_, DesktopBridgeError>(initial.session.clone()),
                &priority_updates,
                &data_updates,
            )
            .await
        );
        let DesktopRuntimeUpdate::ResyncRequired { reason, snapshot } =
            priority_rx.recv().await.unwrap()
        else {
            panic!("data overflow must publish a priority resync request");
        };
        assert_eq!(reason.code, "desktop_data_queue_full");
        assert_eq!(
            snapshot.session.session_id,
            initial.session.session.session_id
        );
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn typed_recovery_reasons_replace_the_projection_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let mut projection = DesktopProjection::new(initial.clone()).unwrap();
        let cursor = initial.session.cursor.clone();

        let live_lag = recovery_update(CodingAgentFreshSnapshotRecovery {
            requested_sequence: cursor.last_event_sequence.saturating_sub(1),
            oldest_available_sequence: cursor.last_event_sequence,
            fresh_cursor: cursor.clone(),
            reason: CodingAgentRecoveryReason::LiveReceiverLag,
            snapshot: Box::new(initial.session.clone()),
        });
        let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &live_lag else {
            panic!("live lag must become a typed resync update");
        };
        assert_eq!(reason.code, "product_event_live_receiver_lag");
        assert!(projection.apply(live_lag).is_replaced());
        assert_eq!(
            projection
                .last_resync_reason()
                .expect("live lag reason should be retained")
                .code,
            "product_event_live_receiver_lag"
        );

        let retained_gap = recovery_update(CodingAgentFreshSnapshotRecovery {
            requested_sequence: 0,
            oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
            fresh_cursor: cursor,
            reason: CodingAgentRecoveryReason::RetainedHistoryGap,
            snapshot: Box::new(initial.session),
        });
        let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &retained_gap else {
            panic!("retained gap must become a typed resync update");
        };
        assert_eq!(reason.code, "product_event_retained_history_gap");
        assert!(projection.apply(retained_gap).is_replaced());
        assert!(projection.recent_events().is_empty());
        assert_eq!(
            projection.apply(DesktopRuntimeUpdate::Stopped),
            DesktopProjectionApply::NoChange
        );
        assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Stopped);
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_state_machine_handles_gap_lag_and_exhaustion_deterministically() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let cursor = initial.session.cursor.clone();

        let retained = CodingAgentFreshSnapshotRecovery {
            requested_sequence: 0,
            oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
            fresh_cursor: cursor.clone(),
            reason: CodingAgentRecoveryReason::RetainedHistoryGap,
            snapshot: Box::new(initial.session.clone()),
        };
        let mut attempts = VecDeque::from([
            DesktopReconnectAttempt::FreshSnapshotRequired(retained),
            DesktopReconnectAttempt::Replayed {
                events: Vec::new(),
                receiver: (),
            },
        ]);
        let mut requested = Vec::new();
        let (events, (), recovery) = establish_reconnect(0, |sequence| {
            requested.push(sequence);
            Ok(attempts
                .pop_front()
                .expect("two reconnect attempts should be consumed"))
        })
        .unwrap();
        assert!(events.is_empty());
        assert_eq!(
            requested,
            vec![0, cursor.last_event_sequence],
            "fresh snapshot cursor must anchor the second reconnect"
        );
        assert_eq!(
            recovery.unwrap().reason,
            CodingAgentRecoveryReason::RetainedHistoryGap
        );

        let first = CodingAgentFreshSnapshotRecovery {
            requested_sequence: 0,
            oldest_available_sequence: 1,
            fresh_cursor: cursor.clone(),
            reason: CodingAgentRecoveryReason::RetainedHistoryGap,
            snapshot: Box::new(initial.session.clone()),
        };
        let second = CodingAgentFreshSnapshotRecovery {
            requested_sequence: cursor.last_event_sequence,
            oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
            fresh_cursor: cursor.clone(),
            reason: CodingAgentRecoveryReason::RetainedHistoryGap,
            snapshot: Box::new(initial.session.clone()),
        };
        let mut attempts = VecDeque::from([
            DesktopReconnectAttempt::<()>::FreshSnapshotRequired(first),
            DesktopReconnectAttempt::<()>::FreshSnapshotRequired(second),
        ]);
        let error = establish_reconnect(0, |_| {
            Ok(attempts
                .pop_front()
                .expect("exhaustion should consume two fresh snapshots"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("reconnect exhausted"));

        let live_lag = CodingAgentFreshSnapshotRecovery {
            requested_sequence: cursor.last_event_sequence.saturating_sub(1),
            oldest_available_sequence: cursor.last_event_sequence,
            fresh_cursor: cursor,
            reason: CodingAgentRecoveryReason::LiveReceiverLag,
            snapshot: Box::new(initial.session),
        };
        let (delivery_tx, delivery_rx) = mpsc::channel(1);
        let mut source = DesktopProductEventSource {
            replay: VecDeque::new(),
            receiver: DesktopProductEventReceiver::Injected(delivery_rx),
        };
        delivery_tx
            .send(Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(
                live_lag,
            )))
            .await
            .unwrap();
        let CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery) =
            source.recv().await.unwrap()
        else {
            panic!("injected live lag must reach the desktop recovery branch");
        };
        let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = recovery_update(recovery) else {
            panic!("live lag delivery must become a typed resync update");
        };
        assert_eq!(reason.code, "product_event_live_receiver_lag");

        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn command_sender_loss_stops_and_joins_the_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, _) = start_runtime(options);
        drop(bridge.commands.take());

        let stopped = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(update) = bridge.next_update().await {
                if matches!(update, DesktopRuntimeUpdate::Stopped) {
                    return;
                }
            }
            panic!("runtime closed without publishing Stopped");
        })
        .await;
        assert!(stopped.is_ok(), "command sender loss did not stop runtime");
        bridge.join_runtime_thread().unwrap();
    }

    #[tokio::test]
    async fn split_runtime_owners_deliver_commands_then_shutdown_and_join() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let initial_session_id = initial.session.session.session_id;
        let (commands, mut events, shutdown) = bridge.into_parts();

        commands.try_reload(60).unwrap();
        let DesktopRuntimeUpdate::Reloaded {
            command_id,
            metadata,
        } = events.next_update().await.unwrap()
        else {
            panic!("the split event owner must deliver the command result");
        };
        assert_eq!(command_id, 60);
        assert_eq!(metadata.session.session.session_id, initial_session_id);

        shutdown.shutdown(&mut events).await.unwrap();
        assert_eq!(
            commands.try_reload(61),
            Err(DesktopCommandAdmissionError::RuntimeClosed),
            "a successful shutdown join must close the independently held command sender"
        );
    }

    #[tokio::test]
    async fn shutdown_deadline_aborts_a_stuck_prompt_task() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let context = CodingAgentEmbeddingContext::load(options).unwrap();
        let mut session = context.create_session().await.unwrap();
        let connection = session
            .connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID))
            .unwrap();
        let requested_after = connection.state().unwrap().cursor.last_event_sequence;
        let (events, pending_recovery) =
            reconnect_event_source(&connection, requested_after).unwrap();
        let task = task::spawn(std::future::pending::<PromptTaskOutput>());
        let active = ActivePrompt {
            command_id: 30,
            operation_id: Some("stuck-operation".into()),
            project: context.snapshot().clone(),
            connection,
            events,
            pending_recovery,
            last_forwarded_sequence: requested_after,
            task,
        };
        let switch = handle_active_command(
            &active,
            DesktopRuntimeCommand::CreateSession { command_id: 31 },
        );
        assert!(matches!(
            switch,
            DesktopRuntimeUpdate::CommandRejected {
                command_id: 31,
                command: DesktopRuntimeCommandKind::CreateSession,
                ref code,
                ..
            } if code == "busy"
        ));
        for (command, expected_kind) in [
            (
                DesktopRuntimeCommand::SelectModel {
                    command_id: 32,
                    model_id: "claude-haiku-4-5".into(),
                },
                DesktopRuntimeCommandKind::SelectModel,
            ),
            (
                DesktopRuntimeCommand::SelectSessionProfile {
                    command_id: 33,
                    profile_id: "review".into(),
                },
                DesktopRuntimeCommandKind::SelectSessionProfile,
            ),
        ] {
            assert!(matches!(
                handle_active_command(&active, command),
                DesktopRuntimeUpdate::CommandRejected {
                    command,
                    ref code,
                    ..
                } if command == expected_kind && code == "busy"
            ));
        }
        let stale_authorization = handle_active_command(
            &active,
            DesktopRuntimeCommand::DecideToolAuthorization {
                command_id: 34,
                identity: ToolAuthorizationIdentity {
                    authorization_id: "already-resolved".into(),
                    operation_id: "stuck-operation".into(),
                    turn_id: "turn-34".into(),
                    tool_call_id: "tool-call-34".into(),
                    capability_generation: 1,
                },
                decision: ToolAuthorizationDecision::Deny { reason: None },
            },
        );
        assert!(matches!(
            stale_authorization,
            DesktopRuntimeUpdate::CommandRejected {
                command_id: 34,
                command: DesktopRuntimeCommandKind::DecideToolAuthorization,
                ref code,
                ..
            } if code == "input"
        ));
        let (priority_updates, mut priority_rx) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);

        shutdown_active_prompt_with_deadline(Some(active), &priority_updates, Duration::ZERO).await;
        let DesktopRuntimeUpdate::RuntimeFailed { error } = priority_rx.recv().await.unwrap()
        else {
            panic!("deadline expiry must publish a runtime failure");
        };
        assert_eq!(error.code, "shutdown_deadline_exceeded");
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn runtime_thread_panic_is_reported_during_join() {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        drop(command_rx);
        let (shutdown, shutdown_rx) = watch::channel(false);
        drop(shutdown_rx);
        let (priority_updates_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        drop(priority_updates_tx);
        let (data_updates_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        drop(data_updates_tx);
        let runtime_thread = thread::spawn(|| panic!("injected desktop runtime panic"));
        let bridge = DesktopRuntimeBridge {
            shutdown: DesktopRuntimeShutdownGuard {
                shutdown,
                runtime_thread: Some(runtime_thread),
            },
            commands: Some(commands),
            events: DesktopRuntimeEventStream {
                priority_updates,
                data_updates,
                pending_priority_update: None,
                pending_data_update: None,
            },
        };

        assert!(matches!(
            bridge.shutdown().await,
            Err(DesktopRuntimeShutdownError::RuntimePanicked)
        ));
    }

    #[tokio::test]
    async fn abort_race_is_typed_and_window_close_is_non_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, _) = start_runtime(options);
        bridge.try_submit_prompt(20, "abort race", None).unwrap();

        let mut saw_control_result = false;
        let mut saw_prompt_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match bridge.next_update().await.unwrap() {
                    DesktopRuntimeUpdate::PromptStarted { .. } => {
                        bridge.try_abort(21).unwrap();
                    }
                    DesktopRuntimeUpdate::ControlAccepted { command_id: 21, .. }
                    | DesktopRuntimeUpdate::CommandRejected { command_id: 21, .. } => {
                        saw_control_result = true
                    }
                    DesktopRuntimeUpdate::PromptFinished { command_id: 20, .. } => {
                        saw_prompt_finished = true
                    }
                    _ => {}
                }
                if saw_control_result && saw_prompt_finished {
                    break;
                }
            }
        })
        .await
        .expect("abort race must converge to a prompt terminal");
        assert!(
            saw_control_result,
            "abort command must receive a typed result"
        );
        assert!(saw_prompt_finished);

        bridge.try_abort(24).unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 24,
                command: DesktopRuntimeCommandKind::Abort,
                ..
            })
        ));

        bridge
            .try_submit_prompt(22, "close during prompt", None)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    bridge.next_update().await,
                    Some(DesktopRuntimeUpdate::PromptAccepted { command_id: 22 })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("terminal ProductEvent acknowledgement must release the next submission slot");
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || drop(bridge)),
        )
        .await
        .expect("dropping the desktop window bridge must return promptly")
        .unwrap();
    }

    #[tokio::test]
    async fn steer_and_follow_up_races_keep_typed_command_association() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, _) = start_runtime(options);
        bridge
            .try_submit_prompt(25, "control association race", None)
            .unwrap();

        let mut controls_sent = false;
        let mut steer_result = false;
        let mut follow_up_result = false;
        let mut prompt_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match bridge.next_update().await.unwrap() {
                    DesktopRuntimeUpdate::PromptStarted { .. } if !controls_sent => {
                        bridge.try_steer(26, "steer exactly").unwrap();
                        bridge.try_follow_up(27, "follow up exactly").unwrap();
                        controls_sent = true;
                    }
                    DesktopRuntimeUpdate::ControlAccepted {
                        command_id: 26,
                        command: DesktopRuntimeCommandKind::Steer,
                        ..
                    }
                    | DesktopRuntimeUpdate::CommandRejected {
                        command_id: 26,
                        command: DesktopRuntimeCommandKind::Steer,
                        ..
                    } => steer_result = true,
                    DesktopRuntimeUpdate::ControlAccepted {
                        command_id: 27,
                        command: DesktopRuntimeCommandKind::FollowUp,
                        ..
                    }
                    | DesktopRuntimeUpdate::CommandRejected {
                        command_id: 27,
                        command: DesktopRuntimeCommandKind::FollowUp,
                        ..
                    } => follow_up_result = true,
                    DesktopRuntimeUpdate::PromptFinished { command_id: 25, .. } => {
                        prompt_finished = true
                    }
                    _ => {}
                }
                if steer_result && follow_up_result && prompt_finished {
                    break;
                }
            }
        })
        .await
        .expect("control races must converge to typed results and a prompt terminal");

        assert!(controls_sent, "controls must be sent after PromptStarted");
        assert!(steer_result, "steer must receive its typed command result");
        assert!(
            follow_up_result,
            "follow-up must receive its typed command result"
        );
        assert!(prompt_finished);
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn authorization_decision_is_typed_and_rejected_without_an_active_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, _) = start_runtime(options);
        let identity = ToolAuthorizationIdentity {
            authorization_id: "authorization-31".into(),
            operation_id: "operation-31".into(),
            turn_id: "turn-31".into(),
            tool_call_id: "tool-call-31".into(),
            capability_generation: 1,
        };
        bridge
            .try_decide_tool_authorization(
                31,
                &identity,
                ToolAuthorizationDecision::Deny {
                    reason: Some("test denial".into()),
                },
            )
            .unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 31,
                command: DesktopRuntimeCommandKind::DecideToolAuthorization,
                ..
            })
        ));
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_actions_are_identity_bound_and_stale_facts_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (mut bridge, initial) = start_runtime(options);
        let pending = CodingAgentRecoveryPending {
            operation_id: "operation-recovery".into(),
            recovery_id: "recovery-id".into(),
            operation_kind: Some("prompt".into()),
            record_version: 3,
            descriptor_revision: 2,
            capability_generation: Some(initial.session.cursor.capability_generation),
            attempt_count: 1,
            last_attempt_at: Some("2026-07-24T00:00:00Z".into()),
            next_attempt_at: None,
        };
        let identity = DesktopRecoveryIdentity::from(&pending);
        let mut projected = initial;
        projected.pending_recoveries = vec![pending];
        let projection = DesktopProjection::new(projected).unwrap();
        let recovery = projection.recoveries().front().unwrap();
        assert!(recovery.authoritative);
        assert_eq!(recovery.identity.as_ref(), Some(&identity));
        assert_eq!(recovery.attempt_count, 1);

        bridge.try_retry_recovery(32, &identity).unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 32,
                command: DesktopRuntimeCommandKind::RetryRecovery,
                ..
            })
        ));
        bridge
            .try_resolve_recovery(33, &identity, CodingAgentRecoveryResolution::Aborted)
            .unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 33,
                command: DesktopRuntimeCommandKind::ResolveRecovery,
                ..
            })
        ));
        bridge.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn authorization_projection_preserves_identity_and_bounds_display_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let (_env, options) = isolated_options(&temp);
        let (bridge, initial) = start_runtime(options);
        let request = ToolAuthorizationRequest {
            authorization_id: "authorization-exact".into(),
            operation_id: "operation-exact".into(),
            turn_id: "turn-exact".into(),
            tool_call_id: "tool-call-exact".into(),
            tool_name: "bash".into(),
            risk: ToolAuthorizationRisk::ShellExecution,
            scope: ToolAuthorizationScope::Shell {
                cwd: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
                command_fingerprint: "fingerprint".into(),
            },
            preview: ToolAuthorizationPreview {
                summary: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
                path: None,
                command: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
                cwd: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
                content_preview: None,
            },
            capability_generation: initial.session.cursor.capability_generation,
            requested_at: "2026-07-24T00:00:00Z".into(),
        };

        let mut bounded = initial.clone();
        bounded.session.pending_authorizations.push(request.clone());
        let projection = DesktopProjection::new(bounded).unwrap();
        let retained = projection
            .snapshot()
            .pending_authorizations
            .first()
            .unwrap();
        assert_eq!(retained.authorization_id, "authorization-exact");
        assert_eq!(retained.operation_id, "operation-exact");
        assert!(retained.preview.summary.len() <= MAX_AUTHORIZATION_TEXT_BYTES);
        assert!(retained.preview.command.as_ref().unwrap().len() <= MAX_AUTHORIZATION_TEXT_BYTES);

        let mut invalid = initial.clone();
        let mut invalid_request = request.clone();
        invalid_request.authorization_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
        invalid.session.pending_authorizations.push(invalid_request);
        assert_eq!(
            DesktopProjection::new(invalid).unwrap_err().code,
            "authorization_identity_invalid"
        );

        let mut stale = initial;
        let mut stale_request = request.clone();
        stale_request.capability_generation =
            stale_request.capability_generation.checked_add(1).unwrap();
        stale.session.pending_authorizations.push(stale_request);
        assert_eq!(
            DesktopProjection::new(stale).unwrap_err().code,
            "authorization_capability_generation_mismatch"
        );

        let identity = request.identity();
        assert_eq!(request.identity(), identity);
        let mut stale_identity = identity.clone();
        stale_identity.capability_generation =
            stale_identity.capability_generation.checked_add(1).unwrap();
        assert_ne!(request.identity(), stale_identity);
        stale_identity = identity;
        stale_identity.operation_id = "another-operation".into();
        assert_ne!(request.identity(), stale_identity);
        bridge.shutdown().await.unwrap();
    }

    #[test]
    fn command_inputs_and_queue_capacities_are_bounded() {
        assert!((1..=128).contains(&DESKTOP_COMMAND_QUEUE_CAPACITY));
        assert!((1..=256).contains(&DESKTOP_UPDATE_QUEUE_CAPACITY));
        assert!((1..=128).contains(&DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY));
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(&"x".repeat(MAX_SESSION_ID_BYTES + 1)).is_err());
        assert!(validate_session_id("session-ok").is_ok());
        assert!(validate_prompt("").is_err());
        assert!(validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert!(validate_prompt("prompt").is_ok());
        assert!(validate_control_text("").is_err());
        assert!(validate_control_text(&"x".repeat(MAX_CONTROL_TEXT_BYTES + 1)).is_err());
        assert!(validate_control_text("steer").is_ok());
        let mut identity = ToolAuthorizationIdentity {
            authorization_id: "authorization-ok".into(),
            operation_id: "operation-ok".into(),
            turn_id: "turn-ok".into(),
            tool_call_id: "tool-call-ok".into(),
            capability_generation: 1,
        };
        assert!(validate_authorization_identity(&identity).is_ok());
        identity.authorization_id.clear();
        assert!(validate_authorization_identity(&identity).is_err());
        identity.authorization_id = "authorization-ok".into();
        identity.tool_call_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
        assert!(validate_authorization_identity(&identity).is_err());
        let mut recovery = DesktopRecoveryIdentity {
            operation_id: "operation-ok".into(),
            recovery_id: "recovery-ok".into(),
            record_version: 1,
            descriptor_revision: 1,
            capability_generation: Some(1),
            attempt_count: 0,
        };
        assert!(validate_recovery_identity(&recovery).is_ok());
        recovery.recovery_id.clear();
        assert!(validate_recovery_identity(&recovery).is_err());
        recovery.recovery_id = "x".repeat(MAX_RECOVERY_ID_BYTES + 1);
        assert!(validate_recovery_identity(&recovery).is_err());
        assert!(validate_selection_id("model", "").is_err());
        assert!(validate_selection_id("profile", &"x".repeat(MAX_SELECTION_ID_BYTES + 1)).is_err());
        assert!(validate_selection_id("model", "claude-haiku-4-5").is_ok());
    }

    #[test]
    fn runtime_error_preserves_only_the_product_safe_error_projection() {
        let product_error = CodingAgentPublicError {
            category: CodingAgentErrorCategory::Provider,
            code: "provider".into(),
            retryable: true,
            summary: "The model provider request failed.".into(),
            context: CodingAgentErrorContext::None,
        };
        let error = runtime_error(&product_error);
        let rendered = format!("{}: {}", error.code, error.message);

        assert_eq!(error.code, "provider");
        assert_eq!(error.message, "The model provider request failed.");
        assert_eq!(rendered, "provider: The model provider request failed.");
    }
}
