use crate::app::error::ApplicationError;
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditCheckOutput, SelfHealingEditDiagnostic, SelfHealingEditRepairAttempt,
};
use serde::{Deserialize, Serialize};

/// Stable reason why client mutation authority is no longer valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentLifecycleRejection {
    #[error("client connection is detached")]
    Detached,
    #[error("client connection generation is stale")]
    StaleGeneration,
    #[error("runtime is shut down")]
    RuntimeShutDown,
}

impl CodingAgentLifecycleRejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::StaleGeneration => "stale_generation",
            Self::RuntimeShutDown => "runtime_shut_down",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CodingSessionError {
    #[error("configuration error: {message}")]
    Config { message: String },
    #[error("invalid input: {message}")]
    Input { message: String },
    #[error("resource error: {message}")]
    Resource { message: String },
    #[error("session error: {message}")]
    Session { message: String },
    #[error("session write rejected before persistence: {message}")]
    SessionWriteRejected { message: String },
    #[error(
        "event stream gap after sequence {requested_after}; oldest available product event is {oldest_available}; client must request a fresh UI snapshot"
    )]
    EventStreamGap {
        requested_after: u64,
        oldest_available: u64,
    },
    #[error("partial commit uncertainty for operation {operation_id}: {message}")]
    PartialCommit {
        operation_id: String,
        message: String,
    },
    #[error(
        "session write blocked by unresolved recovery {recovery_id} for operation {operation_id}"
    )]
    RecoveryPending {
        operation_id: String,
        recovery_id: String,
    },
    #[error("self-healing edit failed: {message}")]
    SelfHealingEditFailed {
        message: String,
        diagnostics: Vec<SelfHealingEditDiagnostic>,
        check_output: Option<Box<SelfHealingEditCheckOutput>>,
        repair_attempts: Vec<SelfHealingEditRepairAttempt>,
    },
    #[error("provider error: {message}")]
    Provider { message: String },
    #[error("tool error: {message}")]
    Tool { message: String },
    #[error("workflow error: {message}")]
    Workflow { message: String },
    #[error("cancelled")]
    Cancelled,
    #[error("unsupported capability: {capability}")]
    UnsupportedCapability { capability: String },
    #[error("busy: {operation}")]
    Busy { operation: String },
    #[error("event stream lagged by {skipped} events; client must request a fresh UI snapshot")]
    EventStreamLag { skipped: u64 },
    #[error(
        "unsupported protocol version for {family}: requested {requested}, supported {supported}"
    )]
    UnsupportedProtocolVersion {
        family: String,
        requested: String,
        supported: String,
    },
    #[error("submission preparation is busy")]
    SubmissionPreparationBusy,
    #[error("prepared submission draft no longer matches")]
    SubmissionDraftMismatch,
    #[error("client capacity exceeded: {limit}")]
    ClientCapacityExceeded { limit: usize },
    #[error("lifecycle rejection: {reason}")]
    Lifecycle {
        reason: CodingAgentLifecycleRejection,
    },
}

impl CodingSessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config { .. } => "config",
            Self::Input { .. } => "input",
            Self::Resource { .. } => "resource",
            Self::Session { .. } => "session",
            Self::SessionWriteRejected { .. } => "session_write_rejected",
            Self::EventStreamGap { .. } => "event_stream_gap",
            Self::PartialCommit { .. } => "partial_commit",
            Self::RecoveryPending { .. } => "recovery_pending",
            Self::SelfHealingEditFailed { .. } => "self_healing_edit_failed",
            Self::Provider { .. } => "provider",
            Self::Tool { .. } => "tool",
            Self::Workflow { .. } => "workflow",
            Self::Cancelled => "cancelled",
            Self::UnsupportedCapability { .. } => "unsupported_capability",
            Self::Busy { .. } => "busy",
            Self::EventStreamLag { .. } => "event_stream_lag",
            Self::UnsupportedProtocolVersion { .. } => "unsupported_protocol_version",
            Self::SubmissionPreparationBusy => "submission_preparation_busy",
            Self::SubmissionDraftMismatch => "submission_draft_mismatch",
            Self::ClientCapacityExceeded { .. } => "client_capacity_exceeded",
            Self::Lifecycle { reason } => reason.code(),
        }
    }
}

impl From<CodingSessionError> for ApplicationError {
    fn from(error: CodingSessionError) -> Self {
        match error {
            CodingSessionError::Config { message }
            | CodingSessionError::Input { message }
            | CodingSessionError::Resource { message }
            | CodingSessionError::Session { message }
            | CodingSessionError::SessionWriteRejected { message }
            | CodingSessionError::SelfHealingEditFailed { message, .. }
            | CodingSessionError::Provider { message }
            | CodingSessionError::Tool { message }
            | CodingSessionError::Workflow { message } => ApplicationError::SessionFailure(message),
            CodingSessionError::PartialCommit {
                operation_id,
                message,
            } => ApplicationError::PartialCommit {
                operation_id,
                message,
            },
            pending @ CodingSessionError::RecoveryPending { .. } => {
                ApplicationError::SessionFailure(pending.to_string())
            }
            gap @ CodingSessionError::EventStreamGap { .. } => {
                ApplicationError::SessionFailure(gap.to_string())
            }
            CodingSessionError::Cancelled => ApplicationError::SessionFailure("cancelled".into()),
            CodingSessionError::UnsupportedCapability { capability } => {
                ApplicationError::UnsupportedMode(capability)
            }
            CodingSessionError::Busy { operation } => {
                ApplicationError::SessionFailure(format!("busy: {operation}"))
            }
            lag @ CodingSessionError::EventStreamLag { .. } => {
                ApplicationError::SessionFailure(lag.to_string())
            }
            version @ CodingSessionError::UnsupportedProtocolVersion { .. } => {
                ApplicationError::SessionFailure(version.to_string())
            }
            other @ (CodingSessionError::SubmissionPreparationBusy
            | CodingSessionError::SubmissionDraftMismatch
            | CodingSessionError::ClientCapacityExceeded { .. }
            | CodingSessionError::Lifecycle { .. }) => {
                ApplicationError::SessionFailure(other.to_string())
            }
        }
    }
}
