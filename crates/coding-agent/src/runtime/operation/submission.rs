use super::OperationExecution;
use super::contract as public_operation;
use super::contract::{CodingAgentOperation, CodingAgentOperationOutcome};
use super::finalize::{FinalizationCommitResult, FinalizationDecision};
use crate::events as event;
use crate::runtime::client::connection as public_connection;
use crate::runtime::client::service::ClientService;
use crate::runtime::facade::{CodingAgentSession, CodingSessionError};
use crate::runtime::public_error::CodingAgentPublicError;
use crate::runtime::snapshot as snapshot_coordinator;
use crate::runtime::snapshot::SnapshotCoordinator;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmissionLeaseLifecycle {
    Prepared,
    Consuming,
    Committed,
    Abandoned,
}

#[derive(Debug)]
pub(crate) struct PendingSubmissionLease {
    handle: snapshot_coordinator::ClientHandle,
    operation_id: String,
    descriptor: public_operation::OperationDescriptor,
    prompt_fingerprint: Option<(String, String)>,
    expected_prompt_draft: Option<snapshot_coordinator::DraftRecord>,
    lifecycle: Arc<Mutex<SubmissionLeaseLifecycle>>,
}

#[derive(Debug)]
pub(super) struct SubmissionCommitGuard {
    client_service: ClientService,
    coordinator: Arc<SnapshotCoordinator>,
    pub(super) handle: snapshot_coordinator::ClientHandle,
    pub(super) operation_id: String,
    pub(super) lifecycle: Arc<Mutex<SubmissionLeaseLifecycle>>,
    pub(super) execution: Option<OperationExecution>,
    pub(super) descriptor: public_operation::OperationDescriptor,
    expected_prompt_draft: Option<snapshot_coordinator::DraftRecord>,
    finished: bool,
}

impl SubmissionCommitGuard {
    pub(super) fn commit_execution(
        &mut self,
        execution: &OperationExecution,
    ) -> Result<(), CodingSessionError> {
        if self.descriptor != execution.descriptor {
            return Err(CodingSessionError::Session {
                message: "admitted operation descriptor changed after submission preparation"
                    .into(),
            });
        }
        execution
            .descriptor
            .validate_terminal_policy()
            .map_err(|message| CodingSessionError::Session {
                message: message.into(),
            })?;
        self.client_service
            .commit_submission_running(
                &self.handle,
                execution.operation_id.clone(),
                execution.descriptor,
                self.expected_prompt_draft.as_ref(),
            )
            .map_err(|error| match error {
                snapshot_coordinator::ClientRegistryError::Lifecycle(reason) => {
                    CodingSessionError::Lifecycle { reason }
                }
                snapshot_coordinator::ClientRegistryError::SubmissionDraftMismatch => {
                    CodingSessionError::SubmissionDraftMismatch
                }
                other => CodingSessionError::Input {
                    message: other.to_string(),
                },
            })?;
        *self.lifecycle.lock().unwrap() = SubmissionLeaseLifecycle::Committed;
        self.execution = Some(execution.clone());
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn commit(&mut self, operation_id: String) -> Result<(), CodingSessionError> {
        self.coordinator
            .register_prepared_submission(&self.handle, operation_id.clone(), self.descriptor)
            .map_err(|error| match error {
                snapshot_coordinator::ClientRegistryError::Lifecycle(reason) => {
                    CodingSessionError::Lifecycle { reason }
                }
                other => CodingSessionError::Input {
                    message: other.to_string(),
                },
            })?;
        let execution = OperationExecution::root(
            self.descriptor.submitted_kind,
            self.descriptor,
            super::OperationOrigin::ClientRoot,
            None,
            None,
            crate::runtime::capability::OperationCapabilitySnapshot::permissive(operation_id),
        );
        self.commit_execution(&execution)
    }

    pub(super) fn finish(
        &mut self,
        decision: &FinalizationDecision,
        commit_result: &FinalizationCommitResult,
    ) -> Result<(), CodingSessionError> {
        if let Some(execution) = &self.execution {
            if decision.operation_id != execution.operation_id
                || decision.root_operation_id
                    != execution
                        .root_operation_id
                        .as_deref()
                        .unwrap_or(&execution.operation_id)
                || decision.descriptor != execution.descriptor
                || decision.parent_operation_id != execution.parent_operation_id
                || decision.session_identity != execution.session_identity
                || decision.operation_kind != execution.kind
                || decision.capability_generation != execution.capability_generation
                || decision.terminal_policy != execution.descriptor.terminal_policy
                || decision.semantic_event_id
                    != format!(
                        "{}/{}/operation_terminal",
                        execution.session_identity.as_deref().unwrap_or("runtime"),
                        execution.operation_id
                    )
            {
                return Err(CodingSessionError::Session {
                    message: "finalization decision does not match admitted operation".into(),
                });
            }
            if let FinalizationCommitResult::InDoubt { recovery_id } = commit_result {
                self.coordinator
                    .mark_recovery_pending(
                        &self.handle,
                        &execution.operation_id,
                        execution.descriptor,
                        recovery_id.clone(),
                    )
                    .map_err(|error| CodingSessionError::Session {
                        message: error.to_string(),
                    })?;
                self.finished = true;
                return Ok(());
            }
            let status = match commit_result {
                FinalizationCommitResult::Committed => decision.terminal_status,
                FinalizationCommitResult::DefinitelyFailed { code, message } => match &decision
                    .payload
                {
                    super::finalize::FinalizationPayload::Failed {
                        code: decision_code,
                        message: decision_message,
                    } if decision_code == code && decision_message == message => {
                        event::ProductEventTerminalStatus::Failed
                    }
                    _ => {
                        return Err(CodingSessionError::Session {
                            message: "definite failure result conflicts with finalization decision"
                                .into(),
                        });
                    }
                },
                FinalizationCommitResult::InDoubt { .. } => unreachable!(),
            };
            match execution.descriptor.terminal_policy {
                public_operation::OperationTerminalPolicy::ProductEvent => {
                    self.coordinator
                        .finalize_terminal_association(
                            &self.handle,
                            &execution.operation_id,
                            execution.descriptor,
                            status,
                        )
                        .map_err(|error| CodingSessionError::Session {
                            message: error.to_string(),
                        })?;
                }
                public_operation::OperationTerminalPolicy::OutcomeAcknowledgement => {
                    let anchor = snapshot_coordinator::SubmittedTerminalAnchor::OutcomeOnly {
                        acknowledgement:
                            public_connection::CodingAgentOutcomeAcknowledgementId::new(format!(
                                "outcome:{}",
                                execution.operation_id
                            )),
                    };
                    self.coordinator
                        .mark_terminal(
                            &self.handle,
                            execution.operation_id.clone(),
                            execution.kind,
                            execution.descriptor,
                            anchor,
                            status,
                        )
                        .map_err(|error| CodingSessionError::Session {
                            message: error.to_string(),
                        })?;
                }
            }
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for SubmissionCommitGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(execution) = self.execution.as_ref() {
            self.coordinator.abort_running_submission_if_matches(
                &self.handle,
                &execution.operation_id,
                execution.descriptor,
            );
        } else if let Ok(mut lifecycle) = self.lifecycle.lock() {
            *lifecycle = SubmissionLeaseLifecycle::Abandoned;
            self.coordinator
                .abandon_prepared_submission(&self.handle, &self.operation_id);
        }
    }
}

impl CodingAgentSession {
    pub async fn run(
        &mut self,
        operation: CodingAgentOperation,
    ) -> Result<CodingAgentOperationOutcome, CodingAgentPublicError> {
        self.run_internal(operation)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn run_internal(
        &mut self,
        operation: CodingAgentOperation,
    ) -> Result<CodingAgentOperationOutcome, CodingSessionError> {
        let _pending_prompt_control_cleanup = matches!(
            &operation,
            CodingAgentOperation::Prompt(_) | CodingAgentOperation::InvokeAgent(_)
        )
        .then(|| self.pending_prompt_control_cleanup_guard())
        .flatten();
        self.runtime_host
            .client_projection
            .snapshots
            .ensure_runtime_running()?;
        let descriptor = operation.descriptor();
        let fingerprint = operation.submission_fingerprint();
        let submission = self.consume_submission_lease(descriptor, fingerprint.as_ref());
        let outcome = self
            .execute_operation_envelope(operation, submission)
            .await?;
        Ok(CodingAgentOperationOutcome::from_internal(outcome))
    }

    pub(crate) fn install_submission_lease(
        &mut self,
        handle: snapshot_coordinator::ClientHandle,
        descriptor: public_operation::OperationDescriptor,
        prompt_fingerprint: Option<(String, String)>,
        expected_prompt_draft: Option<snapshot_coordinator::DraftRecord>,
    ) -> Result<(Arc<Mutex<SubmissionLeaseLifecycle>>, String), CodingSessionError> {
        if let Some(pending) = &self.runtime_host.client_projection.pending_submission {
            let lifecycle = *pending.lifecycle.lock().unwrap();
            if lifecycle != SubmissionLeaseLifecycle::Abandoned
                && self
                    .runtime_host
                    .client_projection
                    .snapshots
                    .is_current(&pending.handle)
            {
                return Err(CodingSessionError::SubmissionPreparationBusy);
            }
        }
        let lifecycle = Arc::new(Mutex::new(SubmissionLeaseLifecycle::Prepared));
        let operation_id = self.next_operation_admission_id();
        self.runtime_host.client_projection.pending_submission = Some(PendingSubmissionLease {
            handle,
            operation_id: operation_id.clone(),
            descriptor,
            prompt_fingerprint,
            expected_prompt_draft,
            lifecycle: lifecycle.clone(),
        });
        Ok((lifecycle, operation_id))
    }

    pub(crate) fn discard_submission_lease(
        &mut self,
        lifecycle: &Arc<Mutex<SubmissionLeaseLifecycle>>,
    ) {
        let matches = self
            .runtime_host
            .client_projection
            .pending_submission
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.lifecycle, lifecycle));
        if matches {
            self.runtime_host.client_projection.pending_submission = None;
        }
    }

    pub(crate) fn owns_submission_coordinator(
        &self,
        coordinator: &Arc<SnapshotCoordinator>,
    ) -> bool {
        Arc::ptr_eq(&self.runtime_host.client_projection.snapshots, coordinator)
    }

    pub(super) fn consume_submission_lease(
        &mut self,
        descriptor: public_operation::OperationDescriptor,
        fingerprint: Option<&(String, String)>,
    ) -> Option<SubmissionCommitGuard> {
        let pending = self
            .runtime_host
            .client_projection
            .pending_submission
            .as_ref()?;
        if *pending.lifecycle.lock().unwrap() == SubmissionLeaseLifecycle::Abandoned {
            self.runtime_host.client_projection.pending_submission = None;
            return None;
        }
        if pending.descriptor != descriptor || pending.prompt_fingerprint.as_ref() != fingerprint {
            return None;
        }
        let pending = self
            .runtime_host
            .client_projection
            .pending_submission
            .take()
            .unwrap();
        *pending.lifecycle.lock().unwrap() = SubmissionLeaseLifecycle::Consuming;
        Some(SubmissionCommitGuard {
            client_service: self.runtime_host.client_projection.clients.clone(),
            coordinator: self.runtime_host.client_projection.snapshots.clone(),
            handle: pending.handle,
            operation_id: pending.operation_id,
            lifecycle: pending.lifecycle,
            execution: None,
            descriptor,
            expected_prompt_draft: pending.expected_prompt_draft,
            finished: false,
        })
    }
}
