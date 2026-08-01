use futures::future::{BoxFuture, FutureExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use agent_core::api::agent::AgentEvent;
use ai::api::conversation::ContentBlock;
use ai::api::stream::AssistantMessageEvent;

use crate::events::CodingAgentSessionWriteFailureStatus;
use crate::events::agent::{AgentInvocationEvent, AgentStreamEvent};
use crate::events::capability::CapabilityEvent;
use crate::events::delegation::{DelegationEvent, DelegationEventContext};
use crate::events::diagnostic::DiagnosticEvent;
use crate::events::emission::ProductEventDraft;
use crate::events::message::MessageEvent;
use crate::events::outbox::DurableOutboxRecord;
use crate::events::prompt::PromptEvent;
use crate::events::prompt_stream::PromptStreamEvent;
use crate::events::recovery::RecoveryPendingEvent;
use crate::events::runtime::RuntimeEvent;
use crate::events::session::{SessionLifecycleEvent, SessionWriteEvent};
use crate::events::team::TeamEvent;
use crate::events::tool::ToolEvent;
use crate::events::workflow::SelfHealingEditEvent;
use crate::events::{CodingAgentProductEventKind, ProductEvent, ProductEventSequence};
use crate::operations::prompt::context::{DelegationRequest, InternalPromptTurnOutcome};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditObserver, SelfHealingEditOutcome, SelfHealingEditRepairAttempt,
};
use crate::runtime::capability::InstalledCapabilityGeneration;
use crate::runtime::facade::{CodingSessionError, ProfileId, ProfileKind};
use crate::runtime::operation::finalize::{FinalizationCommitResult, FinalizationDecision};
use crate::runtime::snapshot::{ClientHandle, ClientRegistryError, SnapshotCoordinator};
use crate::session::service::FinalizedSessionWrite;

const EVENT_CHANNEL_CAPACITY: usize = 128;
pub(crate) const EVENT_RETAINED_CAPACITY: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct EventService {
    product_sender: broadcast::Sender<ProductEvent>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
    deferred_terminal_drafts: Arc<Mutex<HashMap<String, ProductEventDraft>>>,
    retained_capacity: usize,
}

#[derive(Debug, Clone, Default)]
struct ProductEventEmissionContext {
    capability_generation: Option<crate::runtime::capability::CapabilityGeneration>,
    operation_kind: Option<crate::runtime::operation::control::OperationKind>,
    root_operation_id: Option<String>,
}

/// The replay/live cut captured while holding the publication lock.
///
/// The receiver is established before the sequence and retained partition are
/// copied, so an event published after `replayed_through` is observable only
/// through `receiver`, never accidentally omitted between two calls.
#[derive(Debug)]
pub(crate) struct ProductEventRecoveryBoundary {
    pub(crate) replayed_through: ProductEventSequence,
    pub(crate) replay: Vec<ProductEvent>,
    pub(crate) receiver: ProductEventReceiver,
    pub(crate) lifecycle_receiver: tokio::sync::watch::Receiver<u64>,
    pub(crate) lifecycle_epoch: u64,
    pub(crate) capability_generation: u64,
}

#[derive(Debug)]
pub(crate) enum ProductEventRecovery {
    Ready(ProductEventRecoveryBoundary),
    RetainedGap {
        requested_after: ProductEventSequence,
        oldest_available: ProductEventSequence,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct SelfHealingEditEventObserver {
    event_service: EventService,
    operation_id: String,
}

impl SelfHealingEditEventObserver {
    pub(crate) fn new(event_service: EventService, operation_id: impl Into<String>) -> Self {
        Self {
            event_service,
            operation_id: operation_id.into(),
        }
    }
}

impl SelfHealingEditObserver for SelfHealingEditEventObserver {
    fn repair_attempted<'a>(
        &'a self,
        path: &'a str,
        repair: &'a SelfHealingEditRepairAttempt,
    ) -> BoxFuture<'a, ()> {
        async move {
            self.event_service.emit_self_healing_edit_repair_attempted(
                self.operation_id.clone(),
                path,
                repair,
            );
        }
        .boxed()
    }
}

impl EventService {
    pub(crate) fn emit_tool_authorization_required(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationRequired { request }.into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_approved(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationApproved { request, decision }.into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_denied(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        reason: impl Into<String>,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationDenied {
                request,
                reason: reason.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_cancelled(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        reason: impl Into<String>,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationCancelled {
                request,
                reason: reason.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self::with_event_capacities_and_coordinator(
            EVENT_CHANNEL_CAPACITY,
            EVENT_RETAINED_CAPACITY,
            snapshot_coordinator,
        )
    }

    fn with_event_capacities_and_coordinator(
        channel_capacity: usize,
        retained_capacity: usize,
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        let channel_capacity = channel_capacity.max(1);
        let (product_sender, _) = broadcast::channel(channel_capacity);
        Self {
            product_sender,
            snapshot_coordinator,
            deferred_terminal_drafts: Arc::new(Mutex::new(HashMap::new())),
            retained_capacity,
        }
    }

    pub(crate) fn recovery_boundary_after_for_client(
        &self,
        handle: &ClientHandle,
        cursor: ProductEventSequence,
    ) -> Result<ProductEventRecovery, ClientRegistryError> {
        let state = self.snapshot_coordinator.state.lock().unwrap();
        SnapshotCoordinator::validate_client(&state, handle)?;
        Ok(self.recovery_boundary_from_state(&state, cursor))
    }

    fn recovery_boundary_from_state(
        &self,
        state: &crate::runtime::snapshot::SnapshotState,
        cursor: ProductEventSequence,
    ) -> ProductEventRecovery {
        let receiver = ProductEventReceiver {
            inner: self.product_sender.subscribe(),
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            snapshot_coordinator: self.snapshot_coordinator.clone(),
        };
        let oldest_available = state
            .retained_product_events
            .front()
            .map(ProductEvent::sequence_internal);
        if let Some(oldest) = oldest_available
            && cursor < oldest
            && cursor != ProductEventSequence::default()
        {
            return ProductEventRecovery::RetainedGap {
                requested_after: cursor,
                oldest_available: oldest,
            };
        }
        let replayed_through =
            ProductEventSequence::new(state.next_event_sequence.saturating_sub(1));
        let replay = state
            .retained_product_events
            .iter()
            .filter(|event| event.sequence_internal() > cursor)
            .cloned()
            .collect();
        ProductEventRecovery::Ready(ProductEventRecoveryBoundary {
            replayed_through,
            replay,
            receiver,
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            lifecycle_epoch: state.lifecycle_epoch,
            capability_generation: state.capability_generation.get(),
        })
    }

    fn retain_product_event(
        &self,
        state: &mut crate::runtime::snapshot::SnapshotState,
        event: ProductEvent,
    ) {
        if self.retained_capacity == 0 {
            state.dropped_before = Some(event.sequence_internal().next());
            return;
        }
        let dropped = state.retained_product_events.len() == self.retained_capacity;
        if state.retained_product_events.len() == self.retained_capacity {
            state.retained_product_events.pop_front();
        }
        state.retained_product_events.push_back(event);
        if dropped {
            state.dropped_before = state
                .retained_product_events
                .front()
                .map(ProductEvent::sequence_internal);
        }
    }

    fn publish_without_root_terminal(&self, draft: ProductEventDraft) -> ProductEvent {
        self.publish(draft, ProductEventEmissionContext::default(), |_, _| None)
    }

    fn publish_self_healing_edit_event(&self, event: SelfHealingEditEvent) -> ProductEvent {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::runtime::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    fn publish_prompt_event(&self, event: PromptEvent) -> ProductEvent {
        let evidence_source = event.clone();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence_source
                            .root_terminal_evidence(kind)
                            .and_then(|evidence| {
                                crate::runtime::operation::contract::product_terminal_operation(
                                    kind, evidence, status,
                                )
                            })
                    })
                })
            },
        )
    }

    fn publish_agent_invocation_event(&self, event: AgentInvocationEvent) -> ProductEvent {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::runtime::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    fn publish_team_event(&self, event: TeamEvent) -> ProductEvent {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::runtime::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    pub(crate) fn publish_prompt_stream_event(&self, event: PromptStreamEvent) -> ProductEvent {
        self.publish_without_root_terminal(event.into_product_draft())
    }

    fn publish(
        &self,
        draft: ProductEventDraft,
        explicit: ProductEventEmissionContext,
        resolve_terminal: impl FnOnce(
            Option<crate::runtime::operation::control::OperationKind>,
            Option<crate::events::CodingAgentProductEventTerminalStatus>,
        )
            -> Option<crate::events::CodingAgentProductEventTerminalOperation>,
    ) -> ProductEvent {
        let mut state = self.snapshot_coordinator.state.lock().unwrap();
        let operation_context = draft
            .operation_id
            .as_ref()
            .and_then(|operation_id| state.operation_event_contexts.get(operation_id))
            .cloned();
        let capability_generation = explicit.capability_generation.or_else(|| {
            operation_context
                .as_ref()
                .map(|context| context.capability_generation)
        });
        let operation_kind = explicit
            .operation_kind
            .or_else(|| operation_context.as_ref().map(|context| context.kind));
        let terminal_operation = resolve_terminal(operation_kind, draft.terminal_status);
        let sequence = ProductEventSequence::new(state.next_event_sequence);
        state.next_event_sequence += 1;
        let is_runtime_shutdown = matches!(
            &draft.event,
            crate::events::CodingAgentProductEventKind::Runtime(
                crate::events::CodingAgentRuntimeProductEvent::ShutDown
            )
        );
        let product_event = ProductEvent::new(
            state.event_stream_id.clone(),
            sequence,
            draft.event,
            draft.operation_id,
            operation_context
                .as_ref()
                .and_then(|context| context.parent_operation_id.clone()),
            operation_context
                .as_ref()
                .map(|context| context.root_operation_id.clone())
                .or(explicit.root_operation_id),
            draft.session_id,
            capability_generation,
            draft.terminal_status,
            terminal_operation,
            draft.durability,
        );
        if is_runtime_shutdown
            && state.runtime_lifecycle == crate::runtime::snapshot::RuntimeLifecycle::ShuttingDown
        {
            state.shutdown_drain_boundary = Some(sequence);
        }
        SnapshotCoordinator::observe_root_terminal_in_state(&mut state, &product_event);
        SnapshotCoordinator::observe_context_event_in_state(
            &mut state,
            &product_event,
            operation_kind,
        );
        self.retain_product_event(&mut state, product_event.clone());
        drop(state);
        let _ = self.product_sender.send(product_event.clone());
        product_event
    }

    pub(crate) fn emit_session_opened(&self, session_id: impl Into<String>) -> ProductEvent {
        self.publish_without_root_terminal(
            SessionLifecycleEvent::Opened {
                session_id: session_id.into(),
            }
            .into_product_draft(),
        )
    }

    /// Redeliver one committed durable obligation at most once per runtime.
    pub(crate) fn emit_durable_outbox_record(
        &self,
        record: &DurableOutboxRecord,
    ) -> Option<ProductEvent> {
        let mut state = self.snapshot_coordinator.state.lock().unwrap();
        if !state
            .published_outbox_record_ids
            .insert(record.record_id.clone())
        {
            return None;
        }
        drop(state);
        Some(match record.kind {
            crate::events::outbox::DurableOutboxRecordKind::OperationTerminal => self
                .publish_durable_terminal_draft(
                    record.draft.clone(),
                    record
                        .operation_kind
                        .as_deref()
                        .and_then(crate::runtime::operation::control::OperationKind::from_str),
                ),
            crate::events::outbox::DurableOutboxRecordKind::Recovery => {
                self.publish_durable_recovery_pending_draft(record.draft.clone())
            }
            _ => self.publish_without_root_terminal(record.draft.clone()),
        })
    }

    fn publish_durable_recovery_pending_draft(&self, draft: ProductEventDraft) -> ProductEvent {
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
                capability_generation: capability_generation
                    .map(crate::runtime::capability::CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            |_, _| None,
        )
    }

    pub(crate) fn defer_terminal_draft(
        &self,
        operation_id: impl Into<String>,
        draft: ProductEventDraft,
    ) {
        self.deferred_terminal_drafts
            .lock()
            .unwrap()
            .insert(operation_id.into(), draft);
    }

    pub(crate) fn take_deferred_terminal_draft(
        &self,
        operation_id: &str,
    ) -> Option<ProductEventDraft> {
        self.deferred_terminal_drafts
            .lock()
            .unwrap()
            .remove(operation_id)
    }

    pub(crate) fn has_deferred_terminal_draft(&self, operation_id: &str) -> bool {
        self.deferred_terminal_drafts
            .lock()
            .unwrap()
            .contains_key(operation_id)
    }

    fn publish_durable_terminal_draft(
        &self,
        draft: ProductEventDraft,
        operation_kind_hint: Option<crate::runtime::operation::control::OperationKind>,
    ) -> ProductEvent {
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
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::PromptCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptFailed { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::PromptFailed),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptAborted { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::PromptAborted),
            CodingAgentProductEventKind::Session(
                crate::events::CodingAgentSessionProductEvent::CompactionCompleted { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::CompactionCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditCompleted { .. },
            ) => Some(
                crate::runtime::operation::contract::OperationRootTerminalEvidence::SelfHealingEditCompleted,
            ),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. },
            ) => {
                Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::SelfHealingEditFailed)
            }
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. },
            ) => {
                Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::SelfHealingEditAborted)
            }
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationCompleted { .. },
            ) => Some(
                crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentInvocationCompleted,
            ),
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationFailed { .. },
            ) => {
                Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentInvocationFailed)
            }
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationAborted { .. },
            ) => {
                Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentInvocationAborted)
            }
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Completed { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentTeamCompleted),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Failed { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentTeamFailed),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Aborted { .. },
            ) => Some(crate::runtime::operation::contract::OperationRootTerminalEvidence::AgentTeamAborted),
            _ => None,
        };
        self.publish(
            draft,
            ProductEventEmissionContext {
                operation_kind: operation_kind_hint,
                capability_generation: recovery_resolution_generation
                    .map(crate::runtime::capability::CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    if is_recovery_resolution {
                        return operation_kind.and_then(|kind| {
                            crate::runtime::operation::contract::recovery_resolution_terminal_operation(
                                kind, status,
                            )
                        });
                    }
                    let kind = operation_kind.or_else(|| {
                        evidence.map(|evidence| match evidence {
                            crate::runtime::operation::contract::OperationRootTerminalEvidence::CompactionCompleted => {
                                crate::runtime::operation::control::OperationKind::Compact
                            }
                            _ => crate::runtime::operation::control::OperationKind::Prompt,
                        })
                    });
                    kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            let evidence = match (kind, evidence) {
                                (
                                    crate::runtime::operation::control::OperationKind::Compact,
                                    crate::runtime::operation::contract::OperationRootTerminalEvidence::PromptFailed,
                                ) => crate::runtime::operation::contract::OperationRootTerminalEvidence::CompactPromptFailed,
                                _ => evidence,
                            };
                            crate::runtime::operation::contract::product_terminal_operation(
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
        operation_kind: crate::runtime::operation::control::OperationKind,
    ) -> ProductEvent {
        self.publish_durable_terminal_draft(draft, Some(operation_kind))
    }

    pub(crate) fn emit_diagnostic(
        &self,
        operation_id: Option<impl Into<String>>,
        message: impl Into<String>,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            DiagnosticEvent::Diagnostic {
                operation_id: operation_id.map(Into::into),
                message: message.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_capability_changed(
        &self,
        installed: InstalledCapabilityGeneration,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            CapabilityEvent::Changed {
                generation: installed.generation.get(),
                revocation: installed.revocation,
                cancellation_requested_operation_ids: installed
                    .cancellation_requested_operation_ids,
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_runtime_shutdown(&self) -> ProductEvent {
        self.publish_without_root_terminal(RuntimeEvent::ShutDown.into_product_draft())
    }

    pub(crate) fn emit_prompt_started(
        &self,
        operation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> ProductEvent {
        self.publish_prompt_event(PromptEvent::Started {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
        })
    }

    pub(crate) fn emit_events_before_prompt_outcome(&self, events: &[PromptStreamEvent]) {
        for event in events {
            self.publish_prompt_stream_event(event.clone());
        }
    }

    pub(crate) fn session_write_pending_event(
        operation_id: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Pending {
            operation_id: operation_id.into(),
        }
    }

    pub(crate) fn session_write_committed_event(
        operation_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Committed {
            operation_id: operation_id.into(),
            session_id: session_id.into(),
        }
    }

    pub(crate) fn session_write_skipped_event(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Skipped {
            operation_id: operation_id.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn session_write_failed_event(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
        status: CodingAgentSessionWriteFailureStatus,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Failed {
            operation_id: operation_id.into(),
            reason: reason.into(),
            status,
        }
    }

    pub(crate) fn emit_prompt_completed(
        &self,
        operation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> ProductEvent {
        self.publish_prompt_event(PromptEvent::Completed {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
        })
    }

    pub(crate) fn emit_prompt_aborted(
        &self,
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> ProductEvent {
        self.publish_prompt_event(PromptEvent::Aborted {
            operation_id: operation_id.into(),
            reason: reason.into(),
        })
    }

    pub(crate) fn emit_prompt_failed(
        &self,
        operation_id: impl Into<String>,
        error: CodingSessionError,
    ) -> ProductEvent {
        self.publish_prompt_event(PromptEvent::Failed {
            operation_id: operation_id.into(),
            error,
        })
    }

    pub(crate) fn emit_session_write_events(&self, finalized: &FinalizedSessionWrite) {
        for event in &finalized.events {
            self.publish_without_root_terminal(event.clone().into_product_draft());
        }
    }

    pub(crate) fn emit_session_write_pending(&self, finalized: &FinalizedSessionWrite) {
        for event in &finalized.events {
            if event.is_pending() {
                self.publish_without_root_terminal(event.clone().into_product_draft());
            }
        }
    }

    pub(crate) fn emit_session_write_committed(&self, finalized: &FinalizedSessionWrite) {
        for event in &finalized.events {
            if event.is_final() {
                self.publish_without_root_terminal(event.clone().into_product_draft());
            }
        }
    }

    pub(crate) fn emit_prompt_terminal(&self, outcome: &InternalPromptTurnOutcome) {
        match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                turn_id,
                ..
            } => {
                self.emit_prompt_completed(operation_id.clone(), turn_id.clone());
            }
            InternalPromptTurnOutcome::Aborted {
                operation_id,
                reason,
                ..
            } => {
                self.emit_prompt_aborted(operation_id.clone(), reason.clone());
            }
            InternalPromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            } => {
                if !matches!(error, CodingSessionError::PartialCommit { .. }) {
                    self.emit_prompt_failed(operation_id.clone(), error.clone());
                }
            }
        }
    }

    pub(crate) fn prompt_terminal_draft(
        outcome: &InternalPromptTurnOutcome,
    ) -> Option<ProductEventDraft> {
        let draft = match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                turn_id,
                ..
            } => PromptEvent::Completed {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
            }
            .into_product_draft(),
            InternalPromptTurnOutcome::Aborted {
                operation_id,
                reason,
                ..
            } => PromptEvent::Aborted {
                operation_id: operation_id.clone(),
                reason: reason.clone(),
            }
            .into_product_draft(),
            InternalPromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            } if !matches!(error, CodingSessionError::PartialCommit { .. }) => {
                PromptEvent::Failed {
                    operation_id: operation_id.clone(),
                    error: error.clone(),
                }
                .into_product_draft()
            }
            InternalPromptTurnOutcome::Failed { .. } => return None,
        };
        Some(draft)
    }

    pub(crate) fn emit_agent_invocation_started(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> ProductEvent {
        self.publish_agent_invocation_event(AgentInvocationEvent::Started {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn agent_invocation_completed_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Completed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            final_text: final_text.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_invocation_failed_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Failed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            error: error.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_invocation_aborted_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Aborted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            reason: reason.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn emit_agent_team_started(
        &self,
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::Started {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn agent_team_completed_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEventDraft {
        TeamEvent::Completed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            final_text: final_text.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_team_failed_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        TeamEvent::Failed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            error: error.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_team_aborted_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEventDraft {
        TeamEvent::Aborted {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            reason: reason.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn emit_agent_team_member_started(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        profile_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::MemberStarted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            team_id: team_id.into(),
            profile_id: profile_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn emit_agent_team_member_completed(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        profile_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::MemberCompleted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            team_id: team_id.into(),
            profile_id: profile_id.into(),
            final_text: final_text.into(),
        })
    }

    pub(crate) fn emit_prompt_diagnostics(&self, outcome: &InternalPromptTurnOutcome) {
        let (operation_id, diagnostics) = match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                diagnostics,
                ..
            }
            | InternalPromptTurnOutcome::Failed {
                operation_id,
                diagnostics,
                ..
            } => (operation_id, diagnostics),
            InternalPromptTurnOutcome::Aborted { .. } => return,
        };
        for diagnostic in diagnostics {
            self.emit_diagnostic(Some(operation_id.clone()), diagnostic.message.clone());
        }
    }

    pub(crate) fn emit_delegation_approved(&self, request: &DelegationRequest) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Approved {
            context: delegation_event_context(request),
        }))
    }

    pub(crate) fn emit_delegation_rejected(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Rejected {
            context: delegation_event_context(request),
            reason: reason.to_owned(),
        }))
    }

    pub(crate) fn emit_delegation_confirmation_required(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(
            DelegationEvent::ConfirmationRequired {
                context: delegation_event_context(request),
                reason: reason.to_owned(),
            },
        ))
    }

    pub(crate) fn emit_delegation_started(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
    ) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Started {
            context: delegation_event_context(request),
            child_operation_id: child_operation_id.into(),
        }))
    }

    pub(crate) fn emit_delegation_completed(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
        final_text: impl Into<String>,
    ) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(
            DelegationEvent::Completed {
                context: delegation_event_context(request),
                child_operation_id: child_operation_id.into(),
                final_text: final_text.into(),
            },
        ))
    }

    pub(crate) fn emit_delegation_failed(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
        error: CodingSessionError,
    ) -> ProductEvent {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Failed {
            context: delegation_event_context(request),
            child_operation_id: child_operation_id.into(),
            error,
        }))
    }

    pub(crate) fn emit_self_healing_edit_started(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        replacements: usize,
    ) {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::Started {
            operation_id: operation_id.into(),
            path: path.into(),
            replacements,
        });
    }

    pub(crate) fn emit_self_healing_edit_repair_attempted(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        repair: &SelfHealingEditRepairAttempt,
    ) {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::RepairAttempted {
            operation_id: operation_id.into(),
            path: path.into(),
            attempt: repair.attempt,
            replacements: repair.replacements.clone(),
            diagnostics: repair.diagnostics.clone(),
            check_output: repair.check_output.clone(),
        });
    }

    pub(crate) fn self_healing_edit_completed_draft(
        operation_id: impl Into<String>,
        outcome: &SelfHealingEditOutcome,
    ) -> ProductEventDraft {
        SelfHealingEditEvent::Completed {
            operation_id: operation_id.into(),
            path: outcome.path.clone(),
            attempts: outcome.attempts,
            first_changed_line: outcome.first_changed_line,
            check_output: outcome.check_output.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn self_healing_edit_error_draft(
        operation_id: impl Into<String>,
        path: impl Into<String>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        if error == &CodingSessionError::Cancelled {
            SelfHealingEditEvent::Aborted {
                operation_id: operation_id.into(),
                path: path.into(),
                reason: error.to_string(),
            }
        } else {
            SelfHealingEditEvent::Failed {
                operation_id: operation_id.into(),
                path: path.into(),
                error: error.clone(),
            }
        }
        .into_product_draft()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "recovery event emission keeps durable association metadata explicit"
    )]
    pub(crate) fn emit_startup_recovery_pending(
        &self,
        operation_id: impl Into<String>,
        recovery_id: impl Into<String>,
        reason: impl Into<String>,
        session_id: impl Into<String>,
        operation_kind: Option<crate::runtime::operation::control::OperationKind>,
        capability_generation: Option<u64>,
        attempt_count: u32,
        last_attempt_at: Option<String>,
        next_attempt_at: Option<String>,
    ) -> ProductEvent {
        let operation_id = operation_id.into();
        self.publish(
            RecoveryPendingEvent {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.into(),
                reason: reason.into(),
                session_id: session_id.into(),
                record_version: crate::events::recovery::RECOVERY_RECORD_VERSION,
                descriptor_revision:
                    crate::runtime::operation::contract::OPERATION_DESCRIPTOR_REVISION,
                capability_generation,
                attempt_count,
                last_attempt_at,
                next_attempt_at,
            }
            .into_product_draft(),
            ProductEventEmissionContext {
                capability_generation: capability_generation
                    .map(crate::runtime::capability::CapabilityGeneration::new),
                operation_kind,
                root_operation_id: Some(operation_id),
            },
            |_, _| None,
        )
    }

    pub(crate) fn emit_recovery_pending(
        &self,
        decision: &FinalizationDecision,
        commit_result: &FinalizationCommitResult,
    ) -> Option<ProductEvent> {
        let FinalizationCommitResult::InDoubt { recovery_id } = commit_result else {
            return None;
        };
        let session_id = decision.session_identity.clone()?;
        Some(
            self.publish_without_root_terminal(
                RecoveryPendingEvent {
                    operation_id: decision.operation_id.clone(),
                    recovery_id: recovery_id.clone(),
                    reason: "session commit outcome requires recovery inspection".into(),
                    session_id,
                    record_version: crate::events::recovery::RECOVERY_RECORD_VERSION,
                    descriptor_revision: decision.descriptor.revision,
                    capability_generation: Some(decision.capability_generation.get()),
                    attempt_count: 0,
                    last_attempt_at: None,
                    next_attempt_at: None,
                }
                .into_product_draft(),
            ),
        )
    }

    pub(crate) fn emit_committed_recovery_pending_draft(
        &self,
        draft: ProductEventDraft,
        operation_kind: Option<crate::runtime::operation::control::OperationKind>,
        capability_generation: Option<u64>,
    ) -> ProductEvent {
        self.publish(
            draft,
            ProductEventEmissionContext {
                operation_kind,
                capability_generation: capability_generation
                    .map(crate::runtime::capability::CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            |_, _| None,
        )
    }

    pub(crate) fn subscribe_product_events(&self) -> ProductEventReceiver {
        ProductEventReceiver {
            inner: self.product_sender.subscribe(),
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            snapshot_coordinator: self.snapshot_coordinator.clone(),
        }
    }
}

fn delegation_event_context(request: &DelegationRequest) -> DelegationEventContext {
    DelegationEventContext {
        operation_id: request.operation_id.clone(),
        turn_id: request.turn_id.clone(),
        tool_call_id: request.tool_call_id.clone(),
        requesting_profile_id: request.requesting_profile_id.clone(),
        target_kind: request.target_kind,
        target_id: request.target_id.clone(),
        task: request.task.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEventMappingContext {
    operation_id: String,
    turn_id: String,
    assistant_message_id: Option<String>,
    reasoning_duration_millis: Option<u64>,
}

impl AgentEventMappingContext {
    pub(crate) fn new(operation_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
            assistant_message_id: None,
            reasoning_duration_millis: None,
        }
    }

    pub(crate) fn with_assistant_message_id(mut self, message_id: impl Into<String>) -> Self {
        self.assistant_message_id = Some(message_id.into());
        self
    }

    pub(crate) fn with_reasoning_duration_millis(mut self, duration_millis: Option<u64>) -> Self {
        self.reasoning_duration_millis = duration_millis;
        self
    }
}

pub(crate) fn map_agent_event(
    context: &AgentEventMappingContext,
    event: &AgentEvent,
) -> Vec<PromptStreamEvent> {
    match event {
        AgentEvent::TurnStart { turn } => {
            vec![PromptStreamEvent::Agent(AgentStreamEvent::TurnStarted {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                agent_turn: *turn,
            })]
        }
        AgentEvent::BeforeProviderRequest { request } => {
            vec![PromptStreamEvent::Agent(
                AgentStreamEvent::ProviderRequestStarted {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    provider: request.model.provider.clone(),
                    model: request.model.id.clone(),
                    context_window: (request.model.context_window > 0)
                        .then_some(request.model.context_window),
                },
            )]
        }
        AgentEvent::LlmEvent(event) => map_assistant_event(context, event),
        AgentEvent::ToolCallStart {
            tool_call_id,
            tool_name,
            arguments,
        } => vec![PromptStreamEvent::Tool(ToolEvent::Started {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            arguments_json: arguments.to_string(),
        })],
        AgentEvent::ToolCallUpdate {
            tool_call_id,
            tool_name,
            update,
        } => vec![PromptStreamEvent::Tool(ToolEvent::Updated {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            message: content_blocks_text(&update.content),
        })],
        AgentEvent::ToolCallEnd {
            tool_call_id,
            tool_name,
            result,
        } if result.is_error => vec![PromptStreamEvent::Tool(ToolEvent::Failed {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            message: content_blocks_text(&result.content),
        })],
        AgentEvent::ToolCallEnd {
            tool_call_id,
            tool_name,
            result,
        } => {
            let summary = content_blocks_text(&result.content);
            let mut events = vec![PromptStreamEvent::Tool(ToolEvent::Completed {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                name: tool_name.clone(),
                summary: summary.clone(),
            })];
            if let Some(event) =
                map_delegation_tool_event(context, tool_call_id, tool_name, &summary)
            {
                events.push(event);
            }
            events
        }
        AgentEvent::AgentDone { .. } => Vec::new(),
        AgentEvent::AgentError { .. } => Vec::new(),
        AgentEvent::SessionCompacted {
            summary,
            first_kept_message_id,
            tokens_before,
            details: _,
        } => vec![PromptStreamEvent::Runtime(
            RuntimeEvent::CompactionCompleted {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                summary: summary.clone(),
                first_kept_message_id: first_kept_message_id.clone(),
                tokens_before: *tokens_before,
            },
        )],
    }
}

fn map_assistant_event(
    context: &AgentEventMappingContext,
    event: &AssistantMessageEvent,
) -> Vec<PromptStreamEvent> {
    match event {
        AssistantMessageEvent::Start { .. }
        | AssistantMessageEvent::TextStart { .. }
        | AssistantMessageEvent::ThinkingStart { .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Started {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
            })]
        }
        AssistantMessageEvent::TextDelta { delta, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Delta {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                text: delta.clone(),
            })]
        }
        AssistantMessageEvent::ThinkingDelta { delta, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::ThinkingDelta {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                text: delta.clone(),
            })]
        }
        AssistantMessageEvent::Error { .. } => Vec::new(),
        AssistantMessageEvent::Done { message, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Completed {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                final_text: assistant_text(&message.content),
                images: assistant_images(&message.content),
                usage: message.usage.clone(),
                reasoning_duration_millis: context.reasoning_duration_millis,
            })]
        }
        AssistantMessageEvent::TextEnd { .. }
        | AssistantMessageEvent::ThinkingEnd { .. }
        | AssistantMessageEvent::ToolcallStart { .. }
        | AssistantMessageEvent::ToolcallDelta { .. }
        | AssistantMessageEvent::ToolcallEnd { .. } => Vec::new(),
    }
}

fn content_blocks_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.clone(),
            ContentBlock::Thinking { thinking, .. } => thinking.clone(),
            ContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            ContentBlock::ToolCall { name, .. } => format!("[tool_call:{name}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_images(content: &[ContentBlock]) -> Vec<crate::events::CodingAgentImageContent> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { mime_type, data } => {
                Some(crate::events::CodingAgentImageContent {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn map_delegation_tool_event(
    context: &AgentEventMappingContext,
    tool_call_id: &str,
    tool_name: &str,
    summary: &str,
) -> Option<PromptStreamEvent> {
    if !matches!(tool_name, "delegate_agent" | "delegate_team") {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(summary).ok()?;
    let status = value.get("status")?.as_str()?;
    let target_kind = parse_delegation_target_kind(value.get("target_kind")?.as_str()?)?;
    let target_id = ProfileId::new(value.get("target_id")?.as_str()?.to_owned()).ok()?;
    let requesting_profile_id =
        ProfileId::new(value.get("requesting_profile_id")?.as_str()?.to_owned()).ok()?;
    let task = value.get("task")?.as_str()?.to_owned();

    let context = DelegationEventContext {
        operation_id: context.operation_id.clone(),
        turn_id: context.turn_id.clone(),
        tool_call_id: tool_call_id.to_owned(),
        requesting_profile_id,
        target_kind,
        target_id,
        task,
    };

    match status {
        "requested" => Some(PromptStreamEvent::Delegation(DelegationEvent::Requested {
            context,
        })),
        "rejected" => Some(PromptStreamEvent::Delegation(DelegationEvent::Rejected {
            context,
            reason: value
                .get("message")
                .or_else(|| value.get("error"))
                .and_then(|message| message.as_str())
                .unwrap_or("delegation rejected")
                .to_owned(),
        })),
        _ => None,
    }
}

fn parse_delegation_target_kind(kind: &str) -> Option<ProfileKind> {
    match kind {
        "agent" => Some(ProfileKind::Agent),
        "team" => Some(ProfileKind::Team),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct ProductEventReceiver {
    inner: broadcast::Receiver<ProductEvent>,
    lifecycle_receiver: tokio::sync::watch::Receiver<u64>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
}

impl ProductEventReceiver {
    pub(crate) async fn recv(&mut self) -> Result<ProductEvent, CodingSessionError> {
        loop {
            tokio::select! {
                biased;
                event = self.inner.recv() => return event.map_err(map_recv_error),
                changed = self.lifecycle_receiver.changed() => {
                    changed.map_err(|_| CodingSessionError::Cancelled)?;
                    if self.snapshot_coordinator.is_shut_down() {
                        return Err(CodingSessionError::Cancelled);
                    }
                }
            }
        }
    }

    pub(crate) fn try_recv(&mut self) -> Result<Option<ProductEvent>, CodingSessionError> {
        match self.inner.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => {
                if self.snapshot_coordinator.is_shut_down() {
                    Err(CodingSessionError::Cancelled)
                } else {
                    Ok(None)
                }
            }
            Err(broadcast::error::TryRecvError::Closed) => Err(CodingSessionError::Cancelled),
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                Err(CodingSessionError::EventStreamLag { skipped })
            }
        }
    }
}

fn map_recv_error(error: broadcast::error::RecvError) -> CodingSessionError {
    match error {
        broadcast::error::RecvError::Closed => CodingSessionError::Cancelled,
        broadcast::error::RecvError::Lagged(skipped) => {
            CodingSessionError::EventStreamLag { skipped }
        }
    }
}
