use crate::app::error::ApplicationError;
use crate::kernel::error::{CodingAgentLifecycleRejection, CodingSessionError};
use crate::limits::{
    MAX_PUBLIC_DIAGNOSTIC_CODE_BYTES, MAX_PUBLIC_DIAGNOSTIC_SUMMARY_BYTES,
    MAX_PUBLIC_ERROR_CONTEXT_BYTES,
};
use crate::operations::prompt::context::{CodingDiagnostic, CodingDiagnosticSeverity};
use crate::platform::io::redaction::redact_and_bound;
use crate::profiles::{ProfileDiagnostic, ProfileKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentErrorCategory {
    Configuration,
    Authentication,
    Input,
    Resource,
    Session,
    Persistence,
    Provider,
    Tool,
    Workflow,
    Cancellation,
    Capability,
    Concurrency,
    Protocol,
    Recovery,
    Lifecycle,
    Capacity,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentErrorContext {
    None,
    Operation {
        operation_id: String,
    },
    Recovery {
        operation_id: String,
        recovery_id: String,
    },
    EventStreamGap {
        requested_after: u64,
        oldest_available: u64,
    },
    EventStreamLag {
        skipped: u64,
    },
    ProtocolVersion {
        family: Box<str>,
        requested: Box<str>,
        supported: Box<str>,
    },
    Capacity {
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, thiserror::Error)]
#[error("{summary}")]
pub struct CodingAgentPublicError {
    pub category: CodingAgentErrorCategory,
    pub code: String,
    pub retryable: bool,
    pub summary: String,
    pub context: CodingAgentErrorContext,
}

impl CodingAgentPublicError {
    pub fn code(&self) -> &str {
        &self.code
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentPublicDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentPublicDiagnosticOrigin {
    Configuration,
    Profile,
    Runtime,
    Persistence,
    Provider,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentPublicDiagnostic {
    pub severity: CodingAgentPublicDiagnosticSeverity,
    pub code: String,
    pub summary: String,
    pub origin: CodingAgentPublicDiagnosticOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

impl CodingAgentPublicDiagnostic {
    pub(crate) fn new(
        severity: CodingAgentPublicDiagnosticSeverity,
        code: &str,
        summary: &str,
        origin: CodingAgentPublicDiagnosticOrigin,
        operation_id: Option<&str>,
    ) -> Self {
        Self {
            severity,
            code: redact_and_bound(code, MAX_PUBLIC_DIAGNOSTIC_CODE_BYTES),
            summary: safe_public_summary(summary),
            origin,
            operation_id: operation_id.map(public_context_text),
        }
    }

    pub(crate) fn from_runtime_diagnostics(
        diagnostics: &[CodingDiagnostic],
        operation_id: Option<&str>,
    ) -> Vec<Self> {
        diagnostics
            .iter()
            .map(|diagnostic| {
                let severity = match diagnostic.severity {
                    CodingDiagnosticSeverity::Info => CodingAgentPublicDiagnosticSeverity::Info,
                    CodingDiagnosticSeverity::Warning => {
                        CodingAgentPublicDiagnosticSeverity::Warning
                    }
                    CodingDiagnosticSeverity::Error => CodingAgentPublicDiagnosticSeverity::Error,
                };
                Self::new(
                    severity,
                    diagnostic.code.as_deref().unwrap_or("runtime_diagnostic"),
                    &diagnostic.message,
                    CodingAgentPublicDiagnosticOrigin::Runtime,
                    operation_id,
                )
            })
            .collect()
    }

    pub(crate) fn from_profile_diagnostics(diagnostics: &[ProfileDiagnostic]) -> Vec<Self> {
        diagnostics
            .iter()
            .map(Self::from_profile_diagnostic)
            .collect()
    }

    pub(crate) fn from_profile_diagnostic(diagnostic: &ProfileDiagnostic) -> Self {
        let code = match diagnostic.kind {
            ProfileKind::Agent => "agent_profile_diagnostic",
            ProfileKind::Team => "team_profile_diagnostic",
        };
        Self::new(
            CodingAgentPublicDiagnosticSeverity::Warning,
            code,
            &diagnostic.message,
            CodingAgentPublicDiagnosticOrigin::Profile,
            None,
        )
    }
}

pub(crate) fn safe_public_summary(text: &str) -> String {
    redact_and_bound(text, MAX_PUBLIC_DIAGNOSTIC_SUMMARY_BYTES)
}

impl From<&CodingSessionError> for CodingAgentPublicError {
    fn from(error: &CodingSessionError) -> Self {
        let (category, retryable, summary, context) = match error {
            CodingSessionError::Config { .. } => (
                CodingAgentErrorCategory::Configuration,
                false,
                "Product configuration is invalid or unavailable.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Input { .. } => (
                CodingAgentErrorCategory::Input,
                false,
                "The request is invalid.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Resource { .. } => (
                CodingAgentErrorCategory::Resource,
                true,
                "A required product resource is unavailable.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Session { .. } => (
                CodingAgentErrorCategory::Session,
                true,
                "The session operation failed.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::SessionWriteRejected { .. } => (
                CodingAgentErrorCategory::Persistence,
                true,
                "The session write was rejected before persistence.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::SessionWriteFailure { .. } => (
                CodingAgentErrorCategory::Persistence,
                true,
                "Session persistence is temporarily saturated; retry after pending writes drain.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::EventStreamGap {
                requested_after,
                oldest_available,
            } => (
                CodingAgentErrorCategory::Protocol,
                true,
                "The product event cursor is outside the retained window; request a fresh snapshot.",
                CodingAgentErrorContext::EventStreamGap {
                    requested_after: *requested_after,
                    oldest_available: *oldest_available,
                },
            ),
            CodingSessionError::PartialCommit { operation_id, .. } => (
                CodingAgentErrorCategory::Persistence,
                false,
                "The operation has uncertain persistence state and requires recovery.",
                CodingAgentErrorContext::Operation {
                    operation_id: public_context_text(operation_id),
                },
            ),
            CodingSessionError::RecoveryPending {
                operation_id,
                recovery_id,
            } => (
                CodingAgentErrorCategory::Recovery,
                false,
                "The session has an unresolved recovery action.",
                CodingAgentErrorContext::Recovery {
                    operation_id: public_context_text(operation_id),
                    recovery_id: public_context_text(recovery_id),
                },
            ),
            CodingSessionError::SelfHealingEditFailed { .. } => (
                CodingAgentErrorCategory::Tool,
                false,
                "The self-healing edit did not complete successfully.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Provider { .. } => (
                CodingAgentErrorCategory::Provider,
                true,
                "The model provider request failed.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Tool { .. } => (
                CodingAgentErrorCategory::Tool,
                false,
                "Tool execution failed.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Workflow { .. } => (
                CodingAgentErrorCategory::Session,
                true,
                "The workflow operation failed.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Conflict { .. } => (
                CodingAgentErrorCategory::Workflow,
                false,
                "The product workflow failed.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Stale { .. } => (
                CodingAgentErrorCategory::Workflow,
                true,
                "The operation precondition is stale; refresh and retry.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Cancelled => (
                CodingAgentErrorCategory::Cancellation,
                true,
                "The operation was cancelled.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::UnsupportedCapability { .. } => (
                CodingAgentErrorCategory::Capability,
                false,
                "The requested capability is not supported.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::Busy { .. } => (
                CodingAgentErrorCategory::Concurrency,
                true,
                "Another operation currently owns the required product authority.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::EventStreamLag { skipped } => (
                CodingAgentErrorCategory::Protocol,
                true,
                "The product event receiver lagged; request a fresh snapshot.",
                CodingAgentErrorContext::EventStreamLag { skipped: *skipped },
            ),
            CodingSessionError::UnsupportedProtocolVersion {
                family,
                requested,
                supported,
            } => (
                CodingAgentErrorCategory::Protocol,
                false,
                "The requested protocol version is not supported.",
                CodingAgentErrorContext::ProtocolVersion {
                    family: public_context_text(family).into_boxed_str(),
                    requested: public_context_text(requested).into_boxed_str(),
                    supported: public_context_text(supported).into_boxed_str(),
                },
            ),
            CodingSessionError::SubmissionPreparationBusy => (
                CodingAgentErrorCategory::Concurrency,
                true,
                "Submission preparation is already in progress.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::SubmissionDraftMismatch => (
                CodingAgentErrorCategory::Concurrency,
                true,
                "The prepared submission no longer matches the current draft.",
                CodingAgentErrorContext::None,
            ),
            CodingSessionError::ClientCapacityExceeded { limit } => (
                CodingAgentErrorCategory::Capacity,
                true,
                "The product client capacity is exhausted.",
                CodingAgentErrorContext::Capacity { limit: *limit },
            ),
            CodingSessionError::Lifecycle { reason } => (
                CodingAgentErrorCategory::Lifecycle,
                !matches!(reason, CodingAgentLifecycleRejection::RuntimeShutDown),
                match reason {
                    CodingAgentLifecycleRejection::Detached => {
                        "The product client connection is detached."
                    }
                    CodingAgentLifecycleRejection::StaleGeneration => {
                        "The product client connection generation is stale."
                    }
                    CodingAgentLifecycleRejection::RuntimeShutDown => {
                        "The product runtime is shut down."
                    }
                },
                CodingAgentErrorContext::None,
            ),
        };
        Self {
            category,
            code: error.code().to_owned(),
            retryable,
            summary: summary.to_owned(),
            context,
        }
    }
}

impl From<CodingSessionError> for CodingAgentPublicError {
    fn from(error: CodingSessionError) -> Self {
        Self::from(&error)
    }
}

impl From<&ApplicationError> for CodingAgentPublicError {
    fn from(error: &ApplicationError) -> Self {
        if let ApplicationError::Product(error) = error {
            return error.clone();
        }
        let (category, code, retryable, summary, context) = match error {
            ApplicationError::UnsupportedMode(_) => (
                CodingAgentErrorCategory::Capability,
                "unsupported_mode",
                false,
                "The requested mode is not supported.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::MissingPrompt => (
                CodingAgentErrorCategory::Input,
                "missing_prompt",
                false,
                "A prompt is required.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::UnknownModel(_) => (
                CodingAgentErrorCategory::Input,
                "unknown_model",
                false,
                "The requested model is not available.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::InvalidInput(_) => (
                CodingAgentErrorCategory::Input,
                "invalid_input",
                false,
                "The request is invalid.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::SessionFailure(message) if message == "cancelled" => (
                CodingAgentErrorCategory::Cancellation,
                "cancelled",
                true,
                "The operation was cancelled.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::SessionFailure(_) => (
                CodingAgentErrorCategory::Session,
                "session_failure",
                true,
                "The session request failed.",
                CodingAgentErrorContext::None,
            ),
            ApplicationError::PartialCommit { operation_id, .. } => (
                CodingAgentErrorCategory::Persistence,
                "partial_commit",
                false,
                "The operation has uncertain persistence state and requires recovery.",
                CodingAgentErrorContext::Operation {
                    operation_id: public_context_text(operation_id),
                },
            ),
            ApplicationError::Product(_) => {
                unreachable!("typed product errors return before application projection")
            }
        };
        Self {
            category,
            code: code.to_owned(),
            retryable,
            summary: summary.to_owned(),
            context,
        }
    }
}

impl From<ApplicationError> for CodingAgentPublicError {
    fn from(error: ApplicationError) -> Self {
        Self::from(&error)
    }
}

fn public_context_text(text: &str) -> String {
    redact_and_bound(text, MAX_PUBLIC_ERROR_CONTEXT_BYTES)
}
