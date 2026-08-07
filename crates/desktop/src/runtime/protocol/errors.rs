//! Desktop runtime error projection, admission errors, and typed error sources.

use coding_agent::api::error::CodingAgentPublicError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(in crate::runtime) enum DesktopBridgeError {
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
}

impl From<CodingAgentPublicError> for DesktopBridgeError {
    fn from(error: CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}

#[cfg(test)]
impl DesktopBridgeError {
    pub(in crate::runtime) fn cancelled_for_tests() -> Self {
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
pub(in crate::runtime) trait DesktopRuntimeErrorSource {
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
        }
    }
}

pub(in crate::runtime) fn runtime_error(
    error: &impl DesktopRuntimeErrorSource,
) -> DesktopRuntimeError {
    error.project_runtime_error()
}

pub(in crate::runtime) fn local_runtime_error(code: &str, message: &str) -> DesktopRuntimeError {
    DesktopRuntimeError {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}
