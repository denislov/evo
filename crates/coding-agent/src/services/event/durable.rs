use super::*;

impl EventService {
    /// Redeliver one committed durable obligation at most once per runtime.
    pub(crate) fn emit_durable_outbox_record(
        &self,
        record: &DurableOutboxRecord,
    ) -> Result<Option<ProductEvent>, CodingSessionError> {
        let mut state = self
            .snapshot_coordinator
            .state
            .lock_resource("runtime snapshot state")?;
        if !state
            .published_outbox_record_ids
            .insert(record.record_id.clone())
        {
            return Ok(None);
        }
        drop(state);
        Ok(Some(match record.kind {
            crate::events::outbox::DurableOutboxRecordKind::OperationTerminal => self
                .publish_durable_terminal_draft(
                    record.draft.clone(),
                    record
                        .operation_kind
                        .as_deref()
                        .and_then(OperationKind::from_str),
                )?,
            crate::events::outbox::DurableOutboxRecordKind::Recovery => {
                self.publish_durable_recovery_pending_draft(record.draft.clone())?
            }
            _ => self.publish_without_root_terminal(record.draft.clone())?,
        }))
    }

    pub(super) fn publish_durable_recovery_pending_draft(
        &self,
        draft: ProductEventDraft,
    ) -> Result<ProductEvent, CodingSessionError> {
        let capability_generation = match &draft.event {
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                    capability_generation,
                    ..
                },
            ) => *capability_generation,
            _ => None,
        };
        self.publish(
            draft,
            ProductEventEmissionContext {
                capability_generation: capability_generation.map(CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            |_, _| None,
        )
    }

    pub(crate) fn defer_terminal_draft(
        &self,
        operation_id: impl Into<String>,
        draft: ProductEventDraft,
    ) -> Result<(), CodingSessionError> {
        self.deferred_terminal_drafts
            .lock_resource("deferred terminal drafts")?
            .insert(operation_id.into(), draft);
        Ok(())
    }

    pub(crate) fn take_deferred_terminal_draft(
        &self,
        operation_id: &str,
    ) -> Result<Option<ProductEventDraft>, CodingSessionError> {
        Ok(self
            .deferred_terminal_drafts
            .lock_resource("deferred terminal drafts")?
            .remove(operation_id))
    }

    pub(crate) fn has_deferred_terminal_draft(
        &self,
        operation_id: &str,
    ) -> Result<bool, CodingSessionError> {
        Ok(self
            .deferred_terminal_drafts
            .lock_resource("deferred terminal drafts")?
            .contains_key(operation_id))
    }

    pub(super) fn publish_durable_terminal_draft(
        &self,
        draft: ProductEventDraft,
        operation_kind_hint: Option<OperationKind>,
    ) -> Result<ProductEvent, CodingSessionError> {
        let recovery_resolution_generation = match &draft.event {
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                    capability_generation,
                    ..
                },
            ) => *capability_generation,
            _ => None,
        };
        let is_recovery_resolution = matches!(
            &draft.event,
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::OperationRecoveryResolved { .. }
            )
        );
        let evidence = match &draft.event {
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptCompleted { .. },
            ) => Some(OperationRootTerminalEvidence::PromptCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptFailed { .. },
            ) => Some(OperationRootTerminalEvidence::PromptFailed),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptAborted { .. },
            ) => Some(OperationRootTerminalEvidence::PromptAborted),
            CodingAgentProductEventKind::Session(
                crate::events::CodingAgentSessionProductEvent::CompactionCompleted { .. },
            ) => Some(OperationRootTerminalEvidence::CompactionCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditCompleted { .. },
            ) => Some(OperationRootTerminalEvidence::SelfHealingEditCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. },
            ) => Some(OperationRootTerminalEvidence::SelfHealingEditFailed),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. },
            ) => Some(OperationRootTerminalEvidence::SelfHealingEditAborted),
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationCompleted { .. },
            ) => Some(OperationRootTerminalEvidence::AgentInvocationCompleted),
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationFailed { .. },
            ) => Some(OperationRootTerminalEvidence::AgentInvocationFailed),
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationAborted { .. },
            ) => Some(OperationRootTerminalEvidence::AgentInvocationAborted),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Completed { .. },
            ) => Some(OperationRootTerminalEvidence::AgentTeamCompleted),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Failed { .. },
            ) => Some(OperationRootTerminalEvidence::AgentTeamFailed),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Aborted { .. },
            ) => Some(OperationRootTerminalEvidence::AgentTeamAborted),
            _ => None,
        };
        self.publish(
            draft,
            ProductEventEmissionContext {
                operation_kind: operation_kind_hint,
                capability_generation: recovery_resolution_generation
                    .map(CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    if is_recovery_resolution {
                        return operation_kind.and_then(|kind| {
                            crate::application::operation::contract::recovery_resolution_terminal_operation(
                                kind, status,
                            )
                        });
                    }
                    let kind = operation_kind.or_else(|| {
                        evidence.map(|evidence| match evidence {
                            OperationRootTerminalEvidence::CompactionCompleted => {
                                OperationKind::Compact
                            }
                            _ => OperationKind::Prompt,
                        })
                    });
                    kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            let evidence = match (kind, evidence) {
                                (
                                    OperationKind::Compact,
                                    OperationRootTerminalEvidence::PromptFailed,
                                ) => OperationRootTerminalEvidence::CompactPromptFailed,
                                _ => evidence,
                            };
                            crate::application::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    pub(crate) fn emit_committed_terminal_draft(
        &self,
        draft: ProductEventDraft,
        operation_kind: OperationKind,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_durable_terminal_draft(draft, Some(operation_kind))
    }
}
