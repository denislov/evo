use super::contract::{OperationDescriptor, OperationTerminalPolicy};
use super::control::OperationKind;
use super::{OperationExecution, OperationOutcome};
use crate::events::ProductEventTerminalStatus;
use crate::kernel::capability::CapabilityGeneration;
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::InternalPromptTurnOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinalizationPayload {
    Completed,
    Aborted { reason: String },
    Failed { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalizationDecision {
    pub(crate) operation_id: String,
    pub(crate) root_operation_id: String,
    pub(crate) parent_operation_id: Option<String>,
    pub(crate) session_identity: Option<String>,
    pub(crate) operation_kind: OperationKind,
    pub(crate) descriptor: OperationDescriptor,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) terminal_policy: OperationTerminalPolicy,
    pub(crate) terminal_status: ProductEventTerminalStatus,
    pub(crate) semantic_event_id: String,
    pub(crate) payload: FinalizationPayload,
    pub(crate) requires_recovery: bool,
    pub(crate) persistence_error: Option<CodingSessionError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinalizationCommitResult {
    Committed,
    DefinitelyFailed { code: String, message: String },
    InDoubt { recovery_id: String },
}

impl FinalizationDecision {
    pub(crate) fn freeze(
        execution: &OperationExecution,
        result: &Result<OperationOutcome, CodingSessionError>,
    ) -> Self {
        let requires_recovery = Self::requires_recovery(result);
        let payload = Self::payload(result);
        let terminal_status = match &payload {
            FinalizationPayload::Completed => ProductEventTerminalStatus::Completed,
            FinalizationPayload::Aborted { .. } => ProductEventTerminalStatus::Aborted,
            FinalizationPayload::Failed { .. } => ProductEventTerminalStatus::Failed,
        };
        let scope = execution.session_identity.as_deref().unwrap_or("runtime");
        Self {
            operation_id: execution.operation_id.clone(),
            root_operation_id: execution
                .root_operation_id
                .clone()
                .unwrap_or_else(|| execution.operation_id.clone()),
            parent_operation_id: execution.parent_operation_id.clone(),
            session_identity: execution.session_identity.clone(),
            operation_kind: execution.kind,
            descriptor: execution.descriptor,
            capability_generation: execution.capability_generation,
            terminal_policy: execution.descriptor.terminal_policy,
            terminal_status,
            semantic_event_id: format!("{scope}/{}/operation_terminal", execution.operation_id),
            payload,
            requires_recovery,
            persistence_error: Self::persistence_error(result),
        }
    }

    pub(crate) fn resolve_non_session(
        &self,
    ) -> Result<FinalizationCommitResult, CodingSessionError> {
        if self.descriptor.durability.session_if_persistent {
            return Err(CodingSessionError::Session {
                message: "session-durable finalization requires SessionCoordinator".into(),
            });
        }
        if self.requires_recovery {
            return Err(CodingSessionError::Session {
                message: "non-session finalization has no durable recovery owner".into(),
            });
        }
        match &self.payload {
            FinalizationPayload::Failed { code, message } => {
                Ok(FinalizationCommitResult::DefinitelyFailed {
                    code: code.clone(),
                    message: message.clone(),
                })
            }
            FinalizationPayload::Completed | FinalizationPayload::Aborted { .. } => {
                Ok(FinalizationCommitResult::Committed)
            }
        }
    }

    fn payload(result: &Result<OperationOutcome, CodingSessionError>) -> FinalizationPayload {
        match result {
            Ok(
                OperationOutcome::Prompt(InternalPromptTurnOutcome::Aborted { .. })
                | OperationOutcome::ManualCompaction(InternalPromptTurnOutcome::Aborted { .. })
                | OperationOutcome::BranchSummary(InternalPromptTurnOutcome::Aborted { .. }),
            ) => FinalizationPayload::Aborted {
                reason: "operation aborted".into(),
            },
            Ok(
                OperationOutcome::Prompt(InternalPromptTurnOutcome::Failed { error, .. })
                | OperationOutcome::ManualCompaction(InternalPromptTurnOutcome::Failed {
                    error, ..
                })
                | OperationOutcome::BranchSummary(InternalPromptTurnOutcome::Failed {
                    error, ..
                }),
            ) => FinalizationPayload::Failed {
                code: error.code().into(),
                message: format!("operation failed ({})", error.code()),
            },
            Err(CodingSessionError::Cancelled) => FinalizationPayload::Aborted {
                reason: "cancelled".into(),
            },
            Err(error) => FinalizationPayload::Failed {
                code: error.code().into(),
                message: format!("operation failed ({})", error.code()),
            },
            Ok(_) => FinalizationPayload::Completed,
        }
    }

    fn requires_recovery(result: &Result<OperationOutcome, CodingSessionError>) -> bool {
        matches!(
            result,
            Err(CodingSessionError::PartialCommit { .. })
                | Ok(OperationOutcome::Prompt(
                    InternalPromptTurnOutcome::Failed {
                        error: CodingSessionError::PartialCommit { .. },
                        ..
                    }
                ))
                | Ok(OperationOutcome::ManualCompaction(
                    InternalPromptTurnOutcome::Failed {
                        error: CodingSessionError::PartialCommit { .. },
                        ..
                    }
                ))
                | Ok(OperationOutcome::BranchSummary(
                    InternalPromptTurnOutcome::Failed {
                        error: CodingSessionError::PartialCommit { .. },
                        ..
                    }
                ))
        )
    }

    fn persistence_error(
        result: &Result<OperationOutcome, CodingSessionError>,
    ) -> Option<CodingSessionError> {
        match result {
            Err(error @ CodingSessionError::PartialCommit { .. }) => Some(error.clone()),
            Ok(OperationOutcome::Prompt(InternalPromptTurnOutcome::Failed { error, .. }))
            | Ok(OperationOutcome::ManualCompaction(InternalPromptTurnOutcome::Failed {
                error,
                ..
            }))
            | Ok(OperationOutcome::BranchSummary(InternalPromptTurnOutcome::Failed {
                error,
                ..
            })) if matches!(error, CodingSessionError::PartialCommit { .. }) => Some(error.clone()),
            _ => None,
        }
    }
}
