use std::sync::Mutex;

use super::capability::CapabilityGeneration;
use super::facade::CodingSessionError;
use super::finalization::{FinalizationCommitResult, FinalizationDecision, FinalizationPayload};
use crate::events::emission::ProductEventDraft;
use crate::operations::delegation::{
    PendingDelegationConfirmationQueue, PendingDelegationConfirmationState,
};
use crate::operations::prompt::context::DelegationRequest;
use crate::profiles::ProfileId;
use crate::runtime::operation::OperationClass;
use crate::services::session::{ReplayDerivedOwnerState, replay_derived_owner_state};
use crate::session::event::PersistedDelegationStatus;
#[cfg(test)]
use crate::session::id::{Clock, SystemClock};
use crate::session::service::{SessionPersistence, StartupRecoveryMarker};

/// Sole mutable owner of product session state.
#[derive(Debug)]
pub(crate) struct SessionCoordinator {
    pub(super) persistence: SessionPersistence,
    pub(super) pending_delegation_confirmations: PendingDelegationConfirmationQueue,
    pub(super) startup_recovery_markers: Mutex<Vec<StartupRecoveryMarker>>,
}

impl SessionCoordinator {
    /// Fail closed before admitting a new session write when durable recovery
    /// evidence is still unresolved. Recovery controls themselves bypass this
    /// guard and must provide their expected evidence explicitly.
    pub(crate) fn ensure_write_admission(
        &self,
        class: OperationClass,
    ) -> Result<(), CodingSessionError> {
        if class != OperationClass::SessionWriteRoot {
            return Ok(());
        }
        let SessionPersistence::Persistent(service) = &self.persistence else {
            return Ok(());
        };
        let pending = service.inspect_recovery_pending()?;
        if let Some(first) = pending.into_iter().next() {
            return Err(CodingSessionError::RecoveryPending {
                operation_id: first.operation_id,
                recovery_id: first.recovery_id,
            });
        }
        Ok(())
    }

    pub(crate) fn persist_terminal_decision(
        &self,
        decision: &FinalizationDecision,
        draft: ProductEventDraft,
    ) -> Result<(), CodingSessionError> {
        if decision.requires_recovery {
            return Err(decision.persistence_error.clone().unwrap_or(
                CodingSessionError::PartialCommit {
                    operation_id: decision.operation_id.clone(),
                    message: "terminal decision cannot persist while commit is uncertain".into(),
                },
            ));
        }
        match &self.persistence {
            SessionPersistence::Persistent(service) => {
                service.persist_terminal_decision(decision, draft)
            }
            SessionPersistence::NonPersistent(_) => Ok(()),
        }
    }

    pub(crate) fn resolve_finalization(
        &self,
        decision: &FinalizationDecision,
    ) -> Result<FinalizationCommitResult, CodingSessionError> {
        if decision.requires_recovery {
            let SessionPersistence::Persistent(service) = &self.persistence else {
                return Err(CodingSessionError::Session {
                    message: "non-persistent finalization cannot enter durable recovery".into(),
                });
            };
            return service
                .recovery_id_for_uncertain_operation(&decision.operation_id)
                .map(|recovery_id| FinalizationCommitResult::InDoubt { recovery_id })
                .map_err(|error| decision.persistence_error.clone().unwrap_or(error));
        }
        if !decision.descriptor.durability.session_if_persistent
            && let FinalizationPayload::Failed { code, message } = &decision.payload
        {
            return Ok(FinalizationCommitResult::DefinitelyFailed {
                code: code.clone(),
                message: message.clone(),
            });
        }
        Ok(FinalizationCommitResult::Committed)
    }
}

/// Identity-bearing command accepted by the per-session writer.
#[derive(Debug)]
pub(crate) struct SessionWriterCommand {
    pub(super) operation_id: String,
    pub(super) capability_generation: CapabilityGeneration,
    pub(super) mutation: SessionMutation,
}

impl SessionWriterCommand {
    pub(super) fn set_default_agent_profile(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        profile_id: ProfileId,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::SetDefaultAgentProfile { profile_id },
        }
    }

    pub(super) fn switch_active_leaf(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        target_leaf_id: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::SwitchActiveLeaf {
                target_leaf_id: target_leaf_id.into(),
            },
        }
    }

    pub(super) fn set_session_tree_label(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        entry_id: impl Into<String>,
        label: Option<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::SetSessionTreeLabel {
                entry_id: entry_id.into(),
                label,
            },
        }
    }

    pub(super) fn fork_session(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        target_leaf_id: Option<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::ForkSession { target_leaf_id },
        }
    }

    pub(super) fn reject_delegation(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        source_operation_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        now: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::RejectDelegation {
                source_operation_id: source_operation_id.into(),
                tool_call_id: tool_call_id.into(),
                now: now.into(),
                reason: reason.into(),
            },
        }
    }

    pub(crate) fn approve_delegation(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        source_operation_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        now: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::ApproveDelegation {
                source_operation_id: source_operation_id.into(),
                tool_call_id: tool_call_id.into(),
                now: now.into(),
            },
        }
    }

    pub(crate) fn adopt_delegations(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        pending: Vec<PendingDelegationConfirmationState>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::AdoptDelegations { pending },
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the writer command retains every typed folded-delegation association fact"
    )]
    pub(crate) fn record_delegation_folded_update(
        operation_id: impl Into<String>,
        capability_generation: CapabilityGeneration,
        tool_call_id: impl Into<String>,
        requesting_profile_id: ProfileId,
        target_kind: crate::runtime::facade::ProfileKind,
        target_id: ProfileId,
        task: impl Into<String>,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            capability_generation,
            mutation: SessionMutation::RecordDelegationFoldedUpdate {
                tool_call_id: tool_call_id.into(),
                requesting_profile_id,
                target_kind,
                target_id,
                task: task.into(),
                status,
                child_operation_id,
                summary,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum SessionMutation {
    SetDefaultAgentProfile {
        profile_id: ProfileId,
    },
    SwitchActiveLeaf {
        target_leaf_id: String,
    },
    SetSessionTreeLabel {
        entry_id: String,
        label: Option<String>,
    },
    ForkSession {
        target_leaf_id: Option<String>,
    },
    RejectDelegation {
        source_operation_id: String,
        tool_call_id: String,
        now: String,
        reason: String,
    },
    ApproveDelegation {
        source_operation_id: String,
        tool_call_id: String,
        now: String,
    },
    AdoptDelegations {
        pending: Vec<PendingDelegationConfirmationState>,
    },
    RecordDelegationFoldedUpdate {
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: crate::runtime::facade::ProfileKind,
        target_id: ProfileId,
        task: String,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::service::TransientSessionState;

    fn transient_coordinator() -> SessionCoordinator {
        SessionCoordinator {
            persistence: SessionPersistence::NonPersistent(TransientSessionState::new(
                ProfileId::from("default"),
            )),
            pending_delegation_confirmations: Default::default(),
            startup_recovery_markers: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn writer_command_mutates_session_owner_and_returns_typed_reply() {
        let mut coordinator = transient_coordinator();

        let reply = coordinator
            .execute_writer_command(SessionWriterCommand::set_default_agent_profile(
                "op_profile",
                CapabilityGeneration::new(7),
                ProfileId::from("reviewer"),
            ))
            .unwrap();

        assert!(matches!(
            reply,
            SessionWriterReply::DefaultAgentProfile {
                profile_id,
            }
            if profile_id.as_str() == "reviewer"
        ));
        let SessionPersistence::NonPersistent(state) = &coordinator.persistence else {
            unreachable!("fixture is transient")
        };
        assert_eq!(state.default_agent_profile_id.as_str(), "reviewer");
    }

    #[test]
    fn writer_command_rejects_missing_admitted_identity_without_mutation() {
        let mut coordinator = transient_coordinator();

        let error = coordinator
            .execute_writer_command(SessionWriterCommand::set_default_agent_profile(
                "  ",
                CapabilityGeneration::new(1),
                ProfileId::from("reviewer"),
            ))
            .unwrap_err();

        assert!(matches!(error, CodingSessionError::Session { .. }));
        let SessionPersistence::NonPersistent(state) = &coordinator.persistence else {
            unreachable!("fixture is transient")
        };
        assert_eq!(state.default_agent_profile_id.as_str(), "default");
    }
}

#[derive(Debug)]
pub(crate) enum SessionWriterReply {
    DefaultAgentProfile {
        profile_id: ProfileId,
    },
    ActiveLeaf,
    SessionTreeLabel {
        entry_id: String,
        label: Option<String>,
        updated_at: String,
    },
    ForkedSession {
        session_id: String,
    },
    DelegationRejected {
        request: DelegationRequest,
        reason: String,
    },
    DelegationApproved {
        pending: Box<PendingDelegationConfirmationState>,
    },
    DelegationsAdopted {
        diagnostics: Vec<SessionWriterDiagnostic>,
    },
    DelegationFoldedUpdated,
}

#[derive(Debug)]
pub(crate) struct SessionWriterDiagnostic {
    pub(crate) operation_id: Option<String>,
    pub(crate) message: String,
}

impl SessionCoordinator {
    #[cfg(test)]
    pub(super) fn pending_delegation_confirmations(
        &self,
    ) -> Vec<crate::operations::delegation::PendingDelegationConfirmation> {
        crate::operations::delegation::confirmation::active_views(
            &self.pending_delegation_confirmations,
            &SystemClock.now_rfc3339(),
        )
    }

    /// Execute one validated writer command.
    ///
    /// This synchronous entry point is the first migration stage of the writer
    /// protocol. Its `&mut self` authority guarantees one logical writer; the
    /// bounded command transport is added without changing workflow contracts.
    pub(crate) fn execute_writer_command(
        &mut self,
        command: SessionWriterCommand,
    ) -> Result<SessionWriterReply, CodingSessionError> {
        if command.operation_id.trim().is_empty() {
            return Err(CodingSessionError::Session {
                message: "session writer command requires an admitted operation identity".into(),
            });
        }
        let _capability_generation = command.capability_generation;
        match command.mutation {
            SessionMutation::SetDefaultAgentProfile { profile_id } => {
                match &mut self.persistence {
                    SessionPersistence::Persistent(session_service) => {
                        session_service.set_default_agent_profile_id(profile_id.clone())?;
                    }
                    SessionPersistence::NonPersistent(state) => {
                        state.default_agent_profile_id = profile_id.clone();
                    }
                }
                Ok(SessionWriterReply::DefaultAgentProfile { profile_id })
            }
            SessionMutation::SwitchActiveLeaf { target_leaf_id } => {
                let SessionPersistence::Persistent(session_service) = &mut self.persistence else {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability:
                            "active leaf navigation requires a persistent Rust-native session"
                                .into(),
                    });
                };
                session_service.switch_active_leaf(&target_leaf_id, &command.operation_id)?;
                Ok(SessionWriterReply::ActiveLeaf)
            }
            SessionMutation::SetSessionTreeLabel { entry_id, label } => {
                let SessionPersistence::Persistent(session_service) = &mut self.persistence else {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability: "session tree labels require a persistent Rust-native session"
                            .into(),
                    });
                };
                let update =
                    session_service.set_tree_label(&entry_id, label, &command.operation_id)?;
                Ok(SessionWriterReply::SessionTreeLabel {
                    entry_id: update.entry_id,
                    label: update.label,
                    updated_at: update.updated_at,
                })
            }
            SessionMutation::ForkSession { target_leaf_id } => {
                let SessionPersistence::Persistent(session_service) = &self.persistence else {
                    return Err(CodingSessionError::UnsupportedCapability {
                        capability: "fork requires a persistent Rust-native session".into(),
                    });
                };
                let mut forked_service = session_service
                    .fork_current_admitted(target_leaf_id.as_deref(), &command.operation_id)?;
                let owner_state = match replay_derived_owner_state(&mut forked_service) {
                    Ok(owner_state) => owner_state,
                    Err(error) => {
                        return Err(
                            forked_service.cleanup_failed_transition(&command.operation_id, error)
                        );
                    }
                };
                let session_id = forked_service.session_id().to_owned();
                self.install_forked_session(forked_service, owner_state);
                Ok(SessionWriterReply::ForkedSession { session_id })
            }
            SessionMutation::RejectDelegation {
                source_operation_id,
                tool_call_id,
                now,
                reason,
            } => {
                let pending = crate::operations::delegation::confirmation::active_pending(
                    &self.pending_delegation_confirmations,
                    &source_operation_id,
                    &tool_call_id,
                    &now,
                )?;
                let reason = if reason.trim().is_empty() {
                    "delegation rejected by user".to_string()
                } else {
                    reason
                };
                if let SessionPersistence::Persistent(session_service) = &mut self.persistence {
                    session_service.record_delegation_confirmation_rejected(
                        pending.request.operation_id.clone(),
                        pending.request.tool_call_id.clone(),
                        reason.clone(),
                    )?;
                }
                let pending = self
                    .pending_delegation_confirmations
                    .remove_active(&source_operation_id, &tool_call_id, &now)
                    .unwrap_or(pending);
                Ok(SessionWriterReply::DelegationRejected {
                    request: pending.request,
                    reason,
                })
            }
            SessionMutation::ApproveDelegation {
                source_operation_id,
                tool_call_id,
                now,
            } => {
                let pending = crate::operations::delegation::confirmation::active_pending(
                    &self.pending_delegation_confirmations,
                    &source_operation_id,
                    &tool_call_id,
                    &now,
                )?;
                if let SessionPersistence::Persistent(session_service) = &mut self.persistence {
                    session_service.record_delegation_confirmation_approved(
                        pending.request.operation_id.clone(),
                        pending.request.tool_call_id.clone(),
                        command.operation_id.clone(),
                    )?;
                }
                let pending = self
                    .pending_delegation_confirmations
                    .remove_active(&source_operation_id, &tool_call_id, &now)
                    .unwrap_or(pending);
                Ok(SessionWriterReply::DelegationApproved {
                    pending: Box::new(pending),
                })
            }
            SessionMutation::AdoptDelegations { pending } => {
                let mut diagnostics = Vec::new();
                for pending in pending {
                    if self.pending_delegation_confirmations.is_duplicate(&pending) {
                        diagnostics.push(SessionWriterDiagnostic {
                            operation_id: Some(pending.request.operation_id.clone()),
                            message: format!(
                                "duplicate pending delegation confirmation ignored: operation_id={}, tool_call_id={}",
                                pending.request.operation_id, pending.request.tool_call_id
                            ),
                        });
                        continue;
                    }
                    let runtime_seed =
                        crate::operations::delegation::delegation_runtime_seed_from_prompt_options(
                            &pending.prompt_options,
                            pending.child_delegation_depth,
                            &pending.delegation_lineage,
                        )?;
                    if let SessionPersistence::Persistent(session_service) = &mut self.persistence {
                        session_service.record_delegation_confirmation_requested(
                            pending.request.operation_id.clone(),
                            pending.request.turn_id.clone(),
                            pending.request.tool_call_id.clone(),
                            pending.request.requesting_profile_id.clone(),
                            pending.request.target_kind,
                            pending.request.target_id.clone(),
                            pending.request.task.clone(),
                            pending.reason.clone(),
                            runtime_seed,
                        )?;
                    }
                    self.pending_delegation_confirmations.push(pending);
                }
                Ok(SessionWriterReply::DelegationsAdopted { diagnostics })
            }
            SessionMutation::RecordDelegationFoldedUpdate {
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
            } => {
                if let SessionPersistence::Persistent(session_service) = &mut self.persistence {
                    session_service.record_delegation_folded_update(
                        tool_call_id,
                        requesting_profile_id,
                        target_kind,
                        target_id,
                        task,
                        status,
                        child_operation_id,
                        summary,
                    )?;
                }
                Ok(SessionWriterReply::DelegationFoldedUpdated)
            }
        }
    }

    pub(crate) fn shutdown_writer(&self) -> Result<(), CodingSessionError> {
        if let SessionPersistence::Persistent(session_service) = &self.persistence {
            session_service.shutdown_transaction_writer()?;
        }
        Ok(())
    }

    fn install_forked_session(
        &mut self,
        session_service: crate::session::service::SessionService,
        owner_state: ReplayDerivedOwnerState,
    ) {
        self.persistence = SessionPersistence::Persistent(session_service);
        self.pending_delegation_confirmations = owner_state.pending_delegation_confirmations;
        *self.startup_recovery_markers.lock().unwrap() = owner_state.startup_recovery_markers;
    }
}
