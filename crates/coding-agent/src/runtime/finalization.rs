use super::capability::CapabilityGeneration;
use super::control::OperationKind;
use super::facade::CodingSessionError;
use super::operation::{OperationExecution, OperationOutcome};
use super::outcome::{OperationDescriptor, OperationTerminalPolicy};
use crate::events::ProductEventTerminalStatus;
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

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OperationFinalizer;

impl OperationFinalizer {
    pub(crate) fn freeze(
        &self,
        execution: &OperationExecution,
        result: &Result<OperationOutcome, CodingSessionError>,
    ) -> FinalizationDecision {
        let requires_recovery = Self::requires_recovery(result);
        let payload = Self::payload(result);
        let terminal_status = match &payload {
            FinalizationPayload::Completed => ProductEventTerminalStatus::Completed,
            FinalizationPayload::Aborted { .. } => ProductEventTerminalStatus::Aborted,
            FinalizationPayload::Failed { .. } => ProductEventTerminalStatus::Failed,
        };
        let scope = execution.session_identity.as_deref().unwrap_or("runtime");
        FinalizationDecision {
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
        decision: &FinalizationDecision,
    ) -> Result<FinalizationCommitResult, CodingSessionError> {
        if decision.descriptor.durability.session_if_persistent {
            return Err(CodingSessionError::Session {
                message: "session-durable finalization requires SessionCoordinator".into(),
            });
        }
        if decision.requires_recovery {
            return Err(CodingSessionError::Session {
                message: "non-session finalization has no durable recovery owner".into(),
            });
        }
        match &decision.payload {
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

    #[cfg(test)]
    pub(crate) fn terminal_status(
        result: &Result<OperationOutcome, CodingSessionError>,
    ) -> ProductEventTerminalStatus {
        match Self::payload(result) {
            FinalizationPayload::Completed => ProductEventTerminalStatus::Completed,
            FinalizationPayload::Aborted { .. } => ProductEventTerminalStatus::Aborted,
            FinalizationPayload::Failed { .. } => ProductEventTerminalStatus::Failed,
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
