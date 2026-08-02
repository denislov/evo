#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplicationError {
    #[error("unsupported mode: {0}")]
    UnsupportedMode(String),
    #[error("missing prompt")]
    MissingPrompt,
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    SessionFailure(String),
    #[error("{0}")]
    Product(crate::api::error::CodingAgentPublicError),
    #[error("partial commit uncertainty for operation {operation_id}: {message}")]
    PartialCommit {
        operation_id: String,
        message: String,
    },
}

impl From<crate::api::error::CodingAgentPublicError> for ApplicationError {
    fn from(error: crate::api::error::CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}

impl From<crate::kernel::error::CodingSessionError> for ApplicationError {
    fn from(error: crate::kernel::error::CodingSessionError) -> Self {
        use crate::kernel::error::CodingSessionError;

        match error {
            CodingSessionError::Config { message }
            | CodingSessionError::Input { message }
            | CodingSessionError::Resource { message }
            | CodingSessionError::Session { message }
            | CodingSessionError::SessionWriteRejected { message }
            | CodingSessionError::SessionWriteFailure { message, .. }
            | CodingSessionError::SelfHealingEditFailed { message, .. }
            | CodingSessionError::Provider { message }
            | CodingSessionError::Tool { message }
            | CodingSessionError::Workflow { message } => Self::SessionFailure(message),
            CodingSessionError::PartialCommit {
                operation_id,
                message,
            } => Self::PartialCommit {
                operation_id,
                message,
            },
            pending @ CodingSessionError::RecoveryPending { .. }
            | pending @ CodingSessionError::EventStreamGap { .. }
            | pending @ CodingSessionError::EventStreamLag { .. }
            | pending @ CodingSessionError::UnsupportedProtocolVersion { .. } => {
                Self::SessionFailure(pending.to_string())
            }
            CodingSessionError::Cancelled => Self::SessionFailure("cancelled".into()),
            CodingSessionError::UnsupportedCapability { capability } => {
                Self::UnsupportedMode(capability)
            }
            CodingSessionError::Busy { operation } => {
                Self::SessionFailure(format!("busy: {operation}"))
            }
            other @ (CodingSessionError::SubmissionPreparationBusy
            | CodingSessionError::SubmissionDraftMismatch
            | CodingSessionError::ClientCapacityExceeded { .. }
            | CodingSessionError::Lifecycle { .. }) => Self::SessionFailure(other.to_string()),
        }
    }
}
