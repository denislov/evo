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
use crate::events::profile::ProfileEvent;
use crate::events::prompt::PromptEvent;
use crate::events::prompt_stream::PromptStreamEvent;
#[cfg(test)]
use crate::events::recovery::RecoveryEvent;
use crate::events::recovery::RecoveryPendingEvent;
use crate::events::runtime::RuntimeEvent;
#[cfg(test)]
use crate::events::session::SessionCompactionEvent;
use crate::events::session::{SessionLifecycleEvent, SessionWriteEvent};
use crate::events::team::TeamEvent;
use crate::events::tool::ToolEvent;
use crate::events::workflow::SelfHealingEditEvent;
use crate::events::{CodingAgentProductEventKind, ProductEvent, ProductEventSequence};
#[cfg(test)]
use crate::operations::compaction::runner::ManualCompactionOutcome;
use crate::operations::prompt::context::{DelegationRequest, InternalPromptTurnOutcome};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditObserver, SelfHealingEditOutcome, SelfHealingEditRepairAttempt,
};
use crate::runtime::capability::InstalledCapabilityGeneration;
use crate::runtime::facade::{CodingSessionError, ProfileId, ProfileKind};
use crate::runtime::finalization::{FinalizationCommitResult, FinalizationDecision};
use crate::runtime::snapshot::{ClientHandle, ClientRegistryError, SnapshotCoordinator};
use crate::session::service::FinalizedSessionWrite;

const EVENT_CHANNEL_CAPACITY: usize = 128;
pub(crate) const EVENT_RETAINED_CAPACITY: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct EventService {
    product_sender: broadcast::Sender<ProductEvent>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
    deferred_terminal_drafts: Arc<Mutex<HashMap<String, ProductEventDraft>>>,
    #[cfg(test)]
    channel_capacity: usize,
    retained_capacity: usize,
}

#[derive(Debug, Clone, Default)]
struct ProductEventEmissionContext {
    capability_generation: Option<crate::runtime::capability::CapabilityGeneration>,
    operation_kind: Option<crate::runtime::control::OperationKind>,
    root_operation_id: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventBackpressureStatus {
    pub(crate) channel_capacity: usize,
    pub(crate) retained_capacity: usize,
    pub(crate) oldest_retained_sequence: Option<ProductEventSequence>,
    pub(crate) current_sequence: ProductEventSequence,
    pub(crate) dropped_before: Option<ProductEventSequence>,
}

/// The replay/live cut captured while holding the publication lock.
///
/// The receiver is established before the sequence and retained partition are
/// copied, so an event published after `replayed_through` is observable only
/// through `receiver`, never accidentally omitted between two calls.
#[derive(Debug)]
pub(crate) struct ProductEventRecoveryBoundary {
    #[cfg(test)]
    pub(crate) requested_after: ProductEventSequence,
    pub(crate) replayed_through: ProductEventSequence,
    #[cfg(test)]
    pub(crate) oldest_available: Option<ProductEventSequence>,
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

    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_snapshot_coordinator(SnapshotCoordinator::new())
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

    #[cfg(test)]
    fn with_event_capacities(channel_capacity: usize, retained_capacity: usize) -> Self {
        Self::with_event_capacities_and_coordinator(
            channel_capacity,
            retained_capacity,
            SnapshotCoordinator::new(),
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
            #[cfg(test)]
            channel_capacity,
            retained_capacity,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_event_capacity_for_tests(capacity: usize) -> Self {
        Self::with_event_capacities(capacity, capacity)
    }

    #[cfg(test)]
    pub(crate) fn with_event_capacity_and_coordinator_for_tests(
        capacity: usize,
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self::with_event_capacities_and_coordinator(capacity, capacity, snapshot_coordinator)
    }

    #[cfg(test)]
    pub(crate) fn with_event_capacities_and_coordinator_for_tests(
        channel_capacity: usize,
        retained_capacity: usize,
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self::with_event_capacities_and_coordinator(
            channel_capacity,
            retained_capacity,
            snapshot_coordinator,
        )
    }

    #[cfg(test)]
    pub(crate) fn current_product_sequence(&self) -> ProductEventSequence {
        let state = self.snapshot_coordinator.state.lock().unwrap();
        ProductEventSequence::new(state.next_event_sequence.saturating_sub(1))
    }

    #[cfg(test)]
    pub(crate) fn backpressure_status(&self) -> EventBackpressureStatus {
        let state = self.snapshot_coordinator.state.lock().unwrap();
        EventBackpressureStatus {
            channel_capacity: self.channel_capacity,
            retained_capacity: self.retained_capacity,
            oldest_retained_sequence: state
                .retained_product_events
                .front()
                .map(ProductEvent::sequence_internal),
            current_sequence: ProductEventSequence::new(
                state.next_event_sequence.saturating_sub(1),
            ),
            dropped_before: state.dropped_before,
        }
    }

    #[cfg(test)]
    pub(crate) fn product_events_after(
        &self,
        cursor: ProductEventSequence,
    ) -> Result<Vec<ProductEvent>, CodingSessionError> {
        let state = self.snapshot_coordinator.state.lock().unwrap();
        let Some(oldest) = state
            .retained_product_events
            .front()
            .map(ProductEvent::sequence_internal)
        else {
            return Ok(Vec::new());
        };
        if cursor < oldest && cursor != ProductEventSequence::default() {
            return Err(CodingSessionError::EventStreamGap {
                requested_after: cursor.get(),
                oldest_available: oldest.get(),
            });
        }
        Ok(state
            .retained_product_events
            .iter()
            .filter(|event| event.sequence_internal() > cursor)
            .cloned()
            .collect())
    }

    /// Atomically establish a live receiver and copy the retained partition
    /// after `cursor`. No acknowledgement cursor is read or mutated here.
    #[cfg(test)]
    pub(crate) fn recovery_boundary_after(
        &self,
        cursor: ProductEventSequence,
    ) -> ProductEventRecovery {
        let state = self.snapshot_coordinator.state.lock().unwrap();
        self.recovery_boundary_from_state(&state, cursor)
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
            #[cfg(test)]
            requested_after: cursor,
            replayed_through,
            #[cfg(test)]
            oldest_available,
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

    #[cfg(test)]
    fn publish_session_compaction_event(&self, event: SessionCompactionEvent) -> ProductEvent {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        crate::runtime::outcome::product_terminal_operation(kind, evidence, status)
                    })
                })
            },
        )
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
                            crate::runtime::outcome::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    #[cfg(test)]
    fn publish_recovery_event(
        &self,
        event: RecoveryEvent,
        explicit: ProductEventEmissionContext,
    ) -> ProductEvent {
        self.publish(event.into_product_draft(), explicit, |operation_kind, _| {
            operation_kind.and_then(crate::runtime::outcome::recovered_product_terminal_operation)
        })
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
                                crate::runtime::outcome::product_terminal_operation(
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
                            crate::runtime::outcome::product_terminal_operation(
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
                            crate::runtime::outcome::product_terminal_operation(
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
            Option<crate::runtime::control::OperationKind>,
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

    #[cfg(test)]
    pub(crate) fn emit_agent_event(
        &self,
        context: &AgentEventMappingContext,
        event: &AgentEvent,
    ) -> Vec<PromptStreamEvent> {
        let events = map_agent_event(context, event);
        for event in &events {
            self.publish_prompt_stream_event(event.clone());
        }
        events
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
                        .and_then(crate::runtime::control::OperationKind::from_str),
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
        operation_kind_hint: Option<crate::runtime::control::OperationKind>,
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
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::PromptCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptFailed { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::PromptFailed),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::PromptAborted { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::PromptAborted),
            CodingAgentProductEventKind::Session(
                crate::events::CodingAgentSessionProductEvent::CompactionCompleted { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::CompactionCompleted),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditCompleted { .. },
            ) => Some(
                crate::runtime::outcome::OperationRootTerminalEvidence::SelfHealingEditCompleted,
            ),
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. },
            ) => {
                Some(crate::runtime::outcome::OperationRootTerminalEvidence::SelfHealingEditFailed)
            }
            CodingAgentProductEventKind::Workflow(
                crate::events::CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. },
            ) => {
                Some(crate::runtime::outcome::OperationRootTerminalEvidence::SelfHealingEditAborted)
            }
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationCompleted { .. },
            ) => Some(
                crate::runtime::outcome::OperationRootTerminalEvidence::AgentInvocationCompleted,
            ),
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationFailed { .. },
            ) => {
                Some(crate::runtime::outcome::OperationRootTerminalEvidence::AgentInvocationFailed)
            }
            CodingAgentProductEventKind::Agent(
                crate::events::CodingAgentAgentProductEvent::InvocationAborted { .. },
            ) => {
                Some(crate::runtime::outcome::OperationRootTerminalEvidence::AgentInvocationAborted)
            }
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Completed { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::AgentTeamCompleted),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Failed { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::AgentTeamFailed),
            CodingAgentProductEventKind::Team(
                crate::events::CodingAgentTeamProductEvent::Aborted { .. },
            ) => Some(crate::runtime::outcome::OperationRootTerminalEvidence::AgentTeamAborted),
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
                            crate::runtime::outcome::recovery_resolution_terminal_operation(
                                kind, status,
                            )
                        });
                    }
                    let kind = operation_kind.or_else(|| {
                        evidence.map(|evidence| match evidence {
                            crate::runtime::outcome::OperationRootTerminalEvidence::CompactionCompleted => {
                                crate::runtime::control::OperationKind::Compact
                            }
                            _ => crate::runtime::control::OperationKind::Prompt,
                        })
                    });
                    kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            let evidence = match (kind, evidence) {
                                (
                                    crate::runtime::control::OperationKind::Compact,
                                    crate::runtime::outcome::OperationRootTerminalEvidence::PromptFailed,
                                ) => crate::runtime::outcome::OperationRootTerminalEvidence::CompactPromptFailed,
                                _ => evidence,
                            };
                            crate::runtime::outcome::product_terminal_operation(
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
        operation_kind: crate::runtime::control::OperationKind,
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

    pub(crate) fn emit_default_agent_profile_changed(
        &self,
        profile_id: impl Into<ProfileId>,
    ) -> ProductEvent {
        self.publish_without_root_terminal(
            ProfileEvent::DefaultChanged {
                profile_id: profile_id.into(),
            }
            .into_product_draft(),
        )
    }

    #[cfg(test)]
    pub(crate) fn emit_session_compaction_completed(
        &self,
        operation_id: impl Into<String>,
        turn_id: impl Into<String>,
        outcome: &ManualCompactionOutcome,
    ) {
        self.publish_session_compaction_event(SessionCompactionEvent {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
            summary: outcome.summary.clone(),
            first_kept_message_id: outcome.first_kept_message_id.clone(),
            tokens_before: outcome.tokens_before,
        });
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

    #[cfg(test)]
    pub(crate) fn emit_prompt_outcome(&self, outcome: &InternalPromptTurnOutcome) {
        self.emit_prompt_diagnostics(outcome);
        self.emit_prompt_terminal(outcome);
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

    #[cfg(test)]
    pub(crate) fn emit_agent_invocation_completed(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEvent {
        self.publish_agent_invocation_event(AgentInvocationEvent::Completed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            final_text: final_text.into(),
        })
    }

    #[cfg(test)]
    pub(crate) fn emit_agent_invocation_failed(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        error: CodingSessionError,
    ) -> ProductEvent {
        self.publish_agent_invocation_event(AgentInvocationEvent::Failed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            error,
        })
    }

    #[cfg(test)]
    pub(crate) fn emit_agent_invocation_aborted(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEvent {
        self.publish_agent_invocation_event(AgentInvocationEvent::Aborted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            reason: reason.into(),
        })
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

    #[cfg(test)]
    pub(crate) fn emit_agent_team_completed(
        &self,
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::Completed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            final_text: final_text.into(),
        })
    }

    #[cfg(test)]
    pub(crate) fn emit_agent_team_failed(
        &self,
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        error: CodingSessionError,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::Failed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            error,
        })
    }

    #[cfg(test)]
    pub(crate) fn emit_agent_team_aborted(
        &self,
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEvent {
        self.publish_team_event(TeamEvent::Aborted {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            reason: reason.into(),
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

    #[cfg(test)]
    pub(crate) fn emit_self_healing_edit_completed(
        &self,
        operation_id: impl Into<String>,
        outcome: &SelfHealingEditOutcome,
    ) {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::Completed {
            operation_id: operation_id.into(),
            path: outcome.path.clone(),
            attempts: outcome.attempts,
            first_changed_line: outcome.first_changed_line,
            check_output: outcome.check_output.clone(),
        });
    }

    #[cfg(test)]
    pub(crate) fn emit_self_healing_edit_failed(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        error: &CodingSessionError,
    ) {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::Failed {
            operation_id: operation_id.into(),
            path: path.into(),
            error: error.clone(),
        });
    }

    #[cfg(test)]
    pub(crate) fn emit_self_healing_edit_aborted(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::Aborted {
            operation_id: operation_id.into(),
            path: path.into(),
            reason: reason.into(),
        });
    }

    #[cfg(test)]
    pub(crate) fn emit_operation_recovered(
        &self,
        operation_id: impl Into<String>,
        recovery_id: impl Into<String>,
        reason: impl Into<String>,
        session_id: impl Into<String>,
        operation_kind: Option<crate::runtime::control::OperationKind>,
        capability_generation: Option<u64>,
    ) -> ProductEvent {
        let operation_id = operation_id.into();
        self.publish_recovery_event(
            RecoveryEvent {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.into(),
                reason: reason.into(),
                session_id: session_id.into(),
            },
            ProductEventEmissionContext {
                capability_generation: capability_generation
                    .map(crate::runtime::capability::CapabilityGeneration::new),
                operation_kind,
                root_operation_id: Some(operation_id),
            },
        )
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
        operation_kind: Option<crate::runtime::control::OperationKind>,
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
                descriptor_revision: crate::runtime::outcome::OPERATION_DESCRIPTOR_REVISION,
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
        operation_kind: Option<crate::runtime::control::OperationKind>,
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

#[cfg(test)]
mod tests {
    use agent_core::api::agent::{AgentEvent, ProviderRequestSnapshot};
    use agent_core::api::tool::{AgentToolOutput, AgentToolResult};
    use ai::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason, Usage};
    use ai::api::model::{Model, ModelCost, ModelInput};
    use ai::api::stream::{AssistantMessageEvent, StreamOptions};
    use serde_json::json;

    use super::*;
    use crate::events::{
        CodingAgentCapabilityProductEvent, CodingAgentProductEventFamily,
        CodingAgentProductEventKind, CodingAgentWorkflowProductEvent, ProductEventDurability,
        ProductEventSequence, ProductEventTerminalStatus,
    };

    fn assert_owner_draft_eq(actual: Option<ProductEvent>, expected: ProductEventDraft) {
        std::assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.event)
        );
    }

    fn assert_session_write_event_eq(actual: Option<ProductEvent>, expected: SessionWriteEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_prompt_event_eq(actual: Option<ProductEvent>, expected: PromptEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_diagnostic_event_eq(actual: Option<ProductEvent>, expected: DiagnosticEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_session_lifecycle_event_eq(
        actual: Option<ProductEvent>,
        expected: SessionLifecycleEvent,
    ) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_profile_event_eq(actual: Option<ProductEvent>, expected: ProfileEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_capability_event_eq(actual: Option<ProductEvent>, expected: CapabilityEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_agent_invocation_event_eq(
        actual: Option<ProductEvent>,
        expected: AgentInvocationEvent,
    ) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_team_event_eq(actual: Option<ProductEvent>, expected: TeamEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn assert_prompt_stream_event_eq(actual: Option<ProductEvent>, expected: PromptStreamEvent) {
        assert_eq!(
            actual.as_ref().map(ProductEvent::event),
            Some(&expected.into_product_draft().event),
        );
    }

    fn installed_capability(
        generation: u64,
        revocation: crate::runtime::capability::CapabilityRevocationPolicy,
        cancellation_requested_operation_ids: Vec<String>,
    ) -> InstalledCapabilityGeneration {
        InstalledCapabilityGeneration {
            generation: crate::runtime::capability::CapabilityGeneration::new(generation),
            revocation,
            cancellation_requested_operation_ids,
        }
    }

    fn mapping_context() -> AgentEventMappingContext {
        AgentEventMappingContext::new("op_1", "turn_1").with_assistant_message_id("msg_1")
    }

    fn model() -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: "messages".into(),
            provider: "test-provider".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    fn assistant_message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::empty("messages", "test-model");
        message.content.push(ContentBlock::Text {
            text: text.into(),
            text_signature: None,
        });
        message
    }

    #[test]
    fn event_service_wraps_emitted_events_with_sequence_and_typed_payloads() {
        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();

        let first = service.emit_prompt_completed("op_1", "turn_1");
        let second = service
            .clone()
            .emit_capability_changed(installed_capability(
                1,
                crate::runtime::capability::CapabilityRevocationPolicy::FutureOnly,
                Vec::new(),
            ));

        assert_eq!(first.sequence(), 1);
        assert!(!first.stream_id().is_empty());
        assert_eq!(
            first.event().family(),
            CodingAgentProductEventFamily::Workflow
        );
        assert_eq!(first.operation_id(), Some("op_1"));
        assert_eq!(
            first.terminal_status(),
            Some(ProductEventTerminalStatus::Completed)
        );
        assert_eq!(first.durability(), &ProductEventDurability::LiveOnly);
        assert_eq!(first.terminal_operation(), None);
        assert!(matches!(
            first.event(),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. }
            )
        ));
        assert!(matches!(
            receiver
                .try_recv()
                .unwrap()
                .as_ref()
                .map(ProductEvent::event),
            Some(CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. }
            ))
        ));

        assert_eq!(second.sequence(), 2);
        assert_eq!(second.stream_id(), first.stream_id());
        assert_eq!(
            second.event().family(),
            CodingAgentProductEventFamily::Capability
        );
        assert_eq!(second.operation_id(), None);
        assert_eq!(second.terminal_status(), None);
        assert_eq!(
            second.delivery_class(),
            crate::events::CodingAgentProductEventDeliveryClass::Control
        );
        assert_eq!(second.durability(), &ProductEventDurability::LiveOnly);
        assert!(matches!(
            receiver
                .try_recv()
                .unwrap()
                .as_ref()
                .map(ProductEvent::event),
            Some(CodingAgentProductEventKind::Capability(
                CodingAgentCapabilityProductEvent::Changed { .. }
            ))
        ));
    }

    #[test]
    fn root_terminal_metadata_uses_admitted_operation_contract() {
        use crate::events::CodingAgentProductEventTerminalOperationKind;
        use crate::runtime::capability::CapabilityGeneration;
        use crate::runtime::control::{OperationControl, OperationKind};
        use crate::runtime::operation::OperationClass;

        let coordinator = SnapshotCoordinator::new();
        let service = EventService::with_snapshot_coordinator(coordinator.clone());
        let control = OperationControl::with_snapshot_coordinator(coordinator);

        let compact = control
            .begin_root_with_capability_generation(
                OperationClass::SessionWriteRoot,
                OperationKind::Compact,
                "op_compact".into(),
                CapabilityGeneration::new(1),
            )
            .unwrap();
        let compact_failed = service.emit_prompt_failed(
            "op_compact",
            CodingSessionError::Provider {
                message: "summary failed".into(),
            },
        );
        assert_eq!(
            compact_failed.terminal_operation().unwrap().kind,
            CodingAgentProductEventTerminalOperationKind::Compact
        );
        assert_eq!(
            compact_failed.delivery_class(),
            crate::events::CodingAgentProductEventDeliveryClass::Terminal
        );
        drop(compact);

        let prompt = control
            .begin_root_with_capability_generation(
                OperationClass::SessionWriteRoot,
                OperationKind::Prompt,
                "op_prompt".into(),
                CapabilityGeneration::new(1),
            )
            .unwrap();
        let prompt_failed = service.emit_prompt_failed(
            "op_prompt",
            CodingSessionError::Provider {
                message: "prompt failed".into(),
            },
        );
        assert_eq!(
            prompt_failed.terminal_operation().unwrap().kind,
            CodingAgentProductEventTerminalOperationKind::Prompt
        );
        let wrong_evidence = service.publish_session_compaction_event(SessionCompactionEvent {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            summary: "summary".into(),
            first_kept_message_id: "msg_1".into(),
            tokens_before: 10,
        });
        assert_eq!(wrong_evidence.terminal_operation(), None);
        drop(prompt);

        let unadmitted = service.emit_prompt_completed("op_unknown", "turn_unknown");
        assert_eq!(unadmitted.terminal_operation(), None);
    }

    #[test]
    fn product_events_carry_registered_root_and_child_lineage() {
        use crate::runtime::capability::CapabilityGeneration;
        use crate::runtime::control::{OperationControl, OperationKind};
        use crate::runtime::operation::OperationClass;

        let coordinator = SnapshotCoordinator::new();
        let service = EventService::with_snapshot_coordinator(coordinator.clone());
        let control = OperationControl::with_snapshot_coordinator(coordinator.clone());
        let generation = CapabilityGeneration::new(1);
        let root = control
            .begin_root_with_capability_generation(
                OperationClass::NonSessionRoot,
                OperationKind::AgentInvocation,
                "op_root".into(),
                generation,
            )
            .unwrap();
        let child = control
            .begin_child_with_capability_generation(
                OperationKind::AgentInvocation,
                "op_child".into(),
                "op_root".into(),
                generation,
            )
            .unwrap();
        let grandchild = control
            .begin_child_with_capability_generation(
                OperationKind::Prompt,
                "op_grandchild".into(),
                "op_child".into(),
                generation,
            )
            .unwrap();

        let root_event = service.emit_diagnostic(Some("op_root"), "root");
        let child_event = service.emit_diagnostic(Some("op_child"), "child");
        let grandchild_event = service.emit_diagnostic(Some("op_grandchild"), "grandchild");

        assert_eq!(root_event.parent_operation_id(), None);
        assert_eq!(root_event.root_operation_id(), Some("op_root"));
        assert_eq!(child_event.parent_operation_id(), Some("op_root"));
        assert_eq!(child_event.root_operation_id(), Some("op_root"));
        assert_eq!(grandchild_event.parent_operation_id(), Some("op_child"));
        assert_eq!(grandchild_event.root_operation_id(), Some("op_root"));
        let json = serde_json::to_value(&grandchild_event).unwrap();
        assert_eq!(json["parent_operation_id"], "op_child");
        assert_eq!(json["root_operation_id"], "op_root");

        drop(root);
        assert_eq!(
            coordinator
                .state
                .lock()
                .unwrap()
                .operation_event_contexts
                .len(),
            3
        );
        drop(child);
        drop(grandchild);
        assert!(
            coordinator
                .state
                .lock()
                .unwrap()
                .operation_event_contexts
                .is_empty()
        );
    }

    #[test]
    fn product_events_keep_the_operations_admitted_capability_generation() {
        use crate::runtime::capability::CapabilityGeneration;
        use crate::runtime::control::{OperationControl, OperationKind};
        use crate::runtime::operation::OperationClass;

        let coordinator = SnapshotCoordinator::new();
        let service = EventService::with_snapshot_coordinator(coordinator.clone());
        let control = OperationControl::with_snapshot_coordinator(coordinator.clone());
        let mut guard = control
            .begin_root(
                OperationClass::SessionWriteRoot,
                OperationKind::Prompt,
                "op_generation_1".into(),
            )
            .unwrap();
        guard.bind_capability_generation(CapabilityGeneration::new(1));
        assert_eq!(
            coordinator.install_next_capability_generation().unwrap(),
            CapabilityGeneration::new(2)
        );

        let active = service.emit_prompt_started("op_generation_1", "turn_generation_1");
        assert_eq!(active.capability_generation(), Some(1));
        let public = active;
        assert_eq!(public.capability_generation(), Some(1));
        assert_eq!(
            serde_json::to_value(&public).unwrap()["capability_generation"],
            serde_json::json!(1)
        );

        drop(guard);
        let after_release = service.emit_diagnostic(Some("op_generation_1"), "late diagnostic");
        assert_eq!(after_release.capability_generation(), None);
    }

    #[test]
    fn event_service_publishes_one_typed_product_event_stream() {
        let service = EventService::new();
        let mut product_receiver = service.subscribe_product_events();
        let mut second_product_receiver = service.subscribe_product_events();

        let emitted = service.emit_prompt_completed("op_1", "turn_1");

        let product_event = product_receiver
            .try_recv()
            .unwrap()
            .expect("product event is published");
        assert_eq!(product_event, emitted);
        assert_eq!(product_event.sequence(), 1);
        assert!(matches!(
            product_event.event(),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. }
            )
        ));
        assert_eq!(product_event.operation_id(), Some("op_1"));
        assert_eq!(
            product_event.terminal_status(),
            Some(ProductEventTerminalStatus::Completed)
        );
        assert!(matches!(
            second_product_receiver
                .try_recv()
                .unwrap()
                .as_ref()
                .map(ProductEvent::event),
            Some(CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. }
            ))
        ));
        assert_eq!(product_receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn retained_product_events_can_resume_after_cursor() {
        let service = EventService::new();
        service.emit_session_opened("sess_retained");
        service.emit_diagnostic(None::<String>, "ready");

        let retained = service
            .product_events_after(ProductEventSequence::new(1))
            .unwrap();

        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].sequence(), 2);
    }

    #[test]
    fn retained_product_events_report_gap_before_oldest_sequence() {
        let service = EventService::with_event_capacity_for_tests(2);
        for index in 0..4 {
            service.emit_diagnostic(None::<String>, format!("event {index}"));
        }

        let error = service
            .product_events_after(ProductEventSequence::new(1))
            .unwrap_err();

        assert_eq!(error.code(), "event_stream_gap");
        assert!(error.to_string().contains("fresh UI snapshot"));
    }

    #[test]
    fn zero_retained_capacity_keeps_replay_window_empty() {
        let service = EventService::with_event_capacity_for_tests(0);
        service.emit_diagnostic(None::<String>, "ready");

        let retained = service
            .product_events_after(ProductEventSequence::default())
            .unwrap();

        assert!(retained.is_empty());
        assert_eq!(
            service.current_product_sequence(),
            ProductEventSequence::new(1)
        );
    }

    #[test]
    fn concurrent_emitters_retain_events_in_sequence_order() {
        const EMITTERS: usize = 256;

        let service = EventService::with_event_capacity_for_tests(EMITTERS);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(EMITTERS));
        let handles = (0..EMITTERS)
            .map(|index| {
                let service = service.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    service.emit_diagnostic(None::<String>, format!("event {index}"));
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap();
        }

        let retained = service
            .product_events_after(ProductEventSequence::default())
            .unwrap();
        let sequences = retained
            .iter()
            .map(ProductEvent::sequence)
            .collect::<Vec<_>>();

        assert_eq!(sequences, (1..=EMITTERS as u64).collect::<Vec<_>>());
        assert_eq!(
            service.current_product_sequence(),
            ProductEventSequence::new(EMITTERS as u64)
        );
    }

    #[test]
    fn maps_turn_and_provider_request_events() {
        let context = mapping_context();
        assert_eq!(
            map_agent_event(&context, &AgentEvent::TurnStart { turn: 3 }),
            vec![PromptStreamEvent::Agent(AgentStreamEvent::TurnStarted {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                agent_turn: 3,
            })]
        );

        let event = AgentEvent::BeforeProviderRequest {
            request: ProviderRequestSnapshot {
                model: model(),
                context: Context {
                    system_prompt: None,
                    messages: Vec::new(),
                    tools: None,
                },
                stream_options: StreamOptions::default(),
            },
        };

        assert_eq!(
            map_agent_event(&context, &event),
            vec![PromptStreamEvent::Agent(
                AgentStreamEvent::ProviderRequestStarted {
                    operation_id: "op_1".into(),
                    turn_id: "turn_1".into(),
                    provider: "test-provider".into(),
                    model: "test-model".into(),
                    context_window: None,
                },
            )]
        );
    }

    #[test]
    fn maps_assistant_stream_and_done_events() {
        let context = mapping_context();
        let partial = AssistantMessage::empty("messages", "test-model");

        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::TextStart {
                    content_index: 0,
                    partial: partial.clone(),
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::Started {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: "hi".into(),
                    partial,
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::Delta {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
                text: "hi".into(),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::ThinkingStart {
                    content_index: 0,
                    partial: AssistantMessage::empty("messages", "test-model"),
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::Started {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::ThinkingDelta {
                    content_index: 0,
                    delta: "thinking".into(),
                    partial: AssistantMessage::empty("messages", "test-model"),
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::ThinkingDelta {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
                text: "thinking".into(),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: assistant_message("done"),
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::Completed {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
                final_text: "done".into(),
                images: Vec::new(),
                reasoning_duration_millis: None,
                usage: Usage::default(),
            })]
        );
        // AgentDone no longer emits AssistantMessageCompleted — that is now
        // handled by AssistantMessageEvent::Done, which fires for every
        // completed assistant message (including intermediate ToolUse ones).
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::AgentDone {
                    message: assistant_message("done"),
                }
            ),
            Vec::new(),
        );
    }

    #[test]
    fn maps_assistant_images_into_completed_product_content() {
        let context = mapping_context();
        let mut message = assistant_message("caption");
        message.content.push(ContentBlock::Image {
            mime_type: "image/png".into(),
            data: "cG5n".into(),
        });

        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message,
                }),
            ),
            vec![PromptStreamEvent::Message(MessageEvent::Completed {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
                final_text: "caption".into(),
                images: vec![crate::events::CodingAgentImageContent {
                    mime_type: "image/png".into(),
                    data: "cG5n".into(),
                }],
                reasoning_duration_millis: None,
                usage: Usage::default(),
            })]
        );
    }

    #[test]
    fn maps_tool_lifecycle_events() {
        let context = mapping_context();
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::ToolCallStart {
                    tool_call_id: "tool_1".into(),
                    tool_name: "read".into(),
                    arguments: json!({"path": "Cargo.toml"}),
                },
            ),
            vec![PromptStreamEvent::Tool(ToolEvent::Started {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                tool_call_id: "tool_1".into(),
                name: "read".into(),
                arguments_json: r#"{"path":"Cargo.toml"}"#.into(),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::ToolCallUpdate {
                    tool_call_id: "tool_1".into(),
                    tool_name: "read".into(),
                    update: AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "running".into(),
                        text_signature: None,
                    }]),
                },
            ),
            vec![PromptStreamEvent::Tool(ToolEvent::Updated {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                tool_call_id: "tool_1".into(),
                name: "read".into(),
                message: "running".into(),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::ToolCallEnd {
                    tool_call_id: "tool_1".into(),
                    tool_name: "read".into(),
                    result: AgentToolResult::ok(vec![ContentBlock::Text {
                        text: "ok".into(),
                        text_signature: None,
                    }]),
                },
            ),
            vec![PromptStreamEvent::Tool(ToolEvent::Completed {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                tool_call_id: "tool_1".into(),
                name: "read".into(),
                summary: "ok".into(),
            })]
        );
        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::ToolCallEnd {
                    tool_call_id: "tool_1".into(),
                    tool_name: "read".into(),
                    result: AgentToolResult::error("missing"),
                },
            ),
            vec![PromptStreamEvent::Tool(ToolEvent::Failed {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                tool_call_id: "tool_1".into(),
                name: "read".into(),
                message: "missing".into(),
            })]
        );
    }

    #[test]
    fn maps_delegation_tool_result_events() {
        let context = mapping_context();
        let requested = map_agent_event(
            &context,
            &AgentEvent::ToolCallEnd {
                tool_call_id: "tool_delegate_1".into(),
                tool_name: "delegate_agent".into(),
                result: AgentToolResult::ok(vec![ContentBlock::Text {
                    text: serde_json::json!({
                        "status": "requested",
                        "target_kind": "agent",
                        "target_id": "coder",
                        "task": "implement it",
                        "requesting_profile_id": "planner"
                    })
                    .to_string(),
                    text_signature: None,
                }]),
            },
        );
        assert_eq!(
            requested,
            vec![
                PromptStreamEvent::Tool(ToolEvent::Completed {
                    operation_id: "op_1".into(),
                    turn_id: "turn_1".into(),
                    tool_call_id: "tool_delegate_1".into(),
                    name: "delegate_agent".into(),
                    summary: serde_json::json!({
                        "status": "requested",
                        "target_kind": "agent",
                        "target_id": "coder",
                        "task": "implement it",
                        "requesting_profile_id": "planner"
                    })
                    .to_string(),
                }),
                PromptStreamEvent::Delegation(DelegationEvent::Requested {
                    context: DelegationEventContext {
                        operation_id: "op_1".into(),
                        turn_id: "turn_1".into(),
                        tool_call_id: "tool_delegate_1".into(),
                        requesting_profile_id: ProfileId::from("planner"),
                        target_kind: ProfileKind::Agent,
                        target_id: ProfileId::from("coder"),
                        task: "implement it".into(),
                    },
                }),
            ]
        );

        let rejected = map_agent_event(
            &context,
            &AgentEvent::ToolCallEnd {
                tool_call_id: "tool_delegate_2".into(),
                tool_name: "delegate_team".into(),
                result: AgentToolResult::ok(vec![ContentBlock::Text {
                    text: serde_json::json!({
                        "status": "rejected",
                        "target_kind": "team",
                        "target_id": "implementation",
                        "task": "build it",
                        "requesting_profile_id": "planner",
                        "message": "delegation policy max_depth is 0"
                    })
                    .to_string(),
                    text_signature: None,
                }]),
            },
        );
        assert!(matches!(
            rejected.as_slice(),
            [
                PromptStreamEvent::Tool(ToolEvent::Completed { .. }),
                PromptStreamEvent::Delegation(DelegationEvent::Rejected {
                    context: DelegationEventContext {
                        operation_id,
                        turn_id,
                        tool_call_id,
                        requesting_profile_id,
                        target_kind: ProfileKind::Team,
                        target_id,
                        task,
                    },
                    reason,
                }),
            ] if operation_id == "op_1"
                && turn_id == "turn_1"
                && tool_call_id == "tool_delegate_2"
                && requesting_profile_id == &ProfileId::from("planner")
                && target_id == &ProfileId::from("implementation")
                && task == "build it"
                && reason.contains("max_depth")
        ));
    }

    #[test]
    fn excludes_prompt_outcomes_and_maps_compaction_events() {
        let context = mapping_context();
        assert!(
            map_agent_event(
                &context,
                &AgentEvent::AgentError {
                    error: "provider failed".into(),
                },
            )
            .is_empty()
        );

        let mut message = AssistantMessage::empty("messages", "test-model");
        message.error_message = Some("stream failed".into());
        assert!(
            map_agent_event(
                &context,
                &AgentEvent::LlmEvent(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    message,
                }),
            )
            .is_empty()
        );

        assert_eq!(
            map_agent_event(
                &context,
                &AgentEvent::SessionCompacted {
                    summary: "short".into(),
                    first_kept_message_id: "msg_kept".into(),
                    tokens_before: 42,
                    details: None,
                },
            ),
            vec![PromptStreamEvent::Runtime(
                RuntimeEvent::CompactionCompleted {
                    operation_id: "op_1".into(),
                    turn_id: "turn_1".into(),
                    summary: "short".into(),
                    first_kept_message_id: "msg_kept".into(),
                    tokens_before: 42,
                },
            )]
        );
    }

    #[tokio::test]
    async fn event_service_emits_mapped_agent_events() {
        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let context = mapping_context();

        let mapped = service.emit_agent_event(
            &context,
            &AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "hi".into(),
                partial: AssistantMessage::empty("messages", "test-model"),
            }),
        );

        assert_eq!(mapped.len(), 1);
        assert_prompt_stream_event_eq(
            Some(receiver.recv().await.unwrap()),
            PromptStreamEvent::Message(MessageEvent::Delta {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                message_id: Some("msg_1".into()),
                text: "hi".into(),
            }),
        );
    }

    #[test]
    fn event_service_emits_capability_changed_with_generation_and_revocation() {
        use crate::runtime::capability::{
            CapabilityGeneration, CapabilityRevocationPolicy, InstalledCapabilityGeneration,
        };

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let installed = InstalledCapabilityGeneration {
            generation: CapabilityGeneration::new(3),
            revocation: CapabilityRevocationPolicy::FutureOnly,
            cancellation_requested_operation_ids: Vec::new(),
        };

        service.emit_capability_changed(installed);

        assert_capability_event_eq(
            receiver.try_recv().unwrap(),
            CapabilityEvent::Changed {
                generation: 3,
                revocation: CapabilityRevocationPolicy::FutureOnly,
                cancellation_requested_operation_ids: Vec::new(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_session_opened_and_diagnostics() {
        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();

        service.emit_session_opened("sess_1");
        service.emit_diagnostic(Some("op_1"), "profile warning");
        service.emit_diagnostic(None::<String>, "global warning");

        assert_session_lifecycle_event_eq(
            receiver.try_recv().unwrap(),
            SessionLifecycleEvent::Opened {
                session_id: "sess_1".into(),
            },
        );
        assert_diagnostic_event_eq(
            receiver.try_recv().unwrap(),
            DiagnosticEvent::Diagnostic {
                operation_id: Some("op_1".into()),
                message: "profile warning".into(),
            },
        );
        assert_diagnostic_event_eq(
            receiver.try_recv().unwrap(),
            DiagnosticEvent::Diagnostic {
                operation_id: None,
                message: "global warning".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_owner_status_events() {
        use crate::operations::compaction::runner::ManualCompactionOutcome;

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let compaction = ManualCompactionOutcome {
            summary: "short summary".into(),
            first_kept_message_id: "msg_2".into(),
            tokens_before: 42,
            final_message: AssistantMessage::empty("messages", "test-model"),
        };

        service.emit_default_agent_profile_changed(ProfileId::from("reviewer"));
        service.emit_session_compaction_completed("op_1", "turn_1", &compaction);

        assert_profile_event_eq(
            receiver.try_recv().unwrap(),
            ProfileEvent::DefaultChanged {
                profile_id: ProfileId::from("reviewer"),
            },
        );
        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SessionCompactionEvent {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
                summary: "short summary".into(),
                first_kept_message_id: "msg_2".into(),
                tokens_before: 42,
            }
            .into_product_draft(),
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_buffered_events_and_session_write_slices() {
        use crate::session::service::FinalizedSessionWrite;

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let buffered_events = vec![PromptStreamEvent::Agent(AgentStreamEvent::TurnStarted {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            agent_turn: 1,
        })];

        service.emit_events_before_prompt_outcome(&buffered_events);
        assert_prompt_stream_event_eq(
            receiver.try_recv().unwrap(),
            PromptStreamEvent::Agent(AgentStreamEvent::TurnStarted {
                operation_id: "op_prompt".into(),
                turn_id: "turn_prompt".into(),
                agent_turn: 1,
            }),
        );
        assert_eq!(receiver.try_recv().unwrap(), None);

        let finalized = FinalizedSessionWrite {
            events: vec![
                SessionWriteEvent::Pending {
                    operation_id: "op_write".into(),
                },
                SessionWriteEvent::Committed {
                    operation_id: "op_write".into(),
                    session_id: "sess_1".into(),
                },
                SessionWriteEvent::Skipped {
                    operation_id: "op_skip".into(),
                    reason: "disabled".into(),
                },
            ],
            session_id: Some("sess_1".into()),
            leaf_id: Some("leaf_1".into()),
            committed_session_sequence: Some(7),
        };

        service.emit_session_write_pending(&finalized);
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Pending {
                operation_id: "op_write".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);

        service.emit_session_write_committed(&finalized);
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Committed {
                operation_id: "op_write".into(),
                session_id: "sess_1".into(),
            },
        );
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Skipped {
                operation_id: "op_skip".into(),
                reason: "disabled".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);

        service.emit_session_write_events(&finalized);
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Pending {
                operation_id: "op_write".into(),
            },
        );
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Committed {
                operation_id: "op_write".into(),
                session_id: "sess_1".into(),
            },
        );
        assert_session_write_event_eq(
            receiver.try_recv().unwrap(),
            SessionWriteEvent::Skipped {
                operation_id: "op_skip".into(),
                reason: "disabled".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_prompt_lifecycle_events() {
        use crate::operations::prompt::context::{CodingDiagnostic, InternalPromptTurnOutcome};

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let failed_error = CodingSessionError::Provider {
            message: "provider failed".into(),
        };
        let direct_failed_error = CodingSessionError::Provider {
            message: "direct provider failed".into(),
        };

        service.emit_prompt_started("op_1", "turn_1");
        service.emit_prompt_outcome(&InternalPromptTurnOutcome::Success {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            session_id: None,
            leaf_id: None,
            final_text: "done".into(),
            final_message: AssistantMessage::empty("messages", "test-model"),
            diagnostics: vec![CodingDiagnostic::warning("profile warning")],
        });
        service.emit_prompt_outcome(&InternalPromptTurnOutcome::Failed {
            operation_id: "op_2".into(),
            turn_id: Some("turn_2".into()),
            error: failed_error.clone(),
            diagnostics: vec![CodingDiagnostic::error("provider diagnostic")],
        });
        service.emit_prompt_outcome(&InternalPromptTurnOutcome::Aborted {
            operation_id: "op_3".into(),
            turn_id: Some("turn_3".into()),
            reason: "cancelled".into(),
            session_id: None,
        });
        service.emit_prompt_completed("op_direct_1", "turn_direct_1");
        service.emit_prompt_failed("op_direct_2", direct_failed_error.clone());
        service.emit_prompt_aborted("op_direct_3", "stopped");

        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Started {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
            },
        );
        assert_diagnostic_event_eq(
            receiver.try_recv().unwrap(),
            DiagnosticEvent::Diagnostic {
                operation_id: Some("op_1".into()),
                message: "profile warning".into(),
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Completed {
                operation_id: "op_1".into(),
                turn_id: "turn_1".into(),
            },
        );
        assert_diagnostic_event_eq(
            receiver.try_recv().unwrap(),
            DiagnosticEvent::Diagnostic {
                operation_id: Some("op_2".into()),
                message: "provider diagnostic".into(),
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Failed {
                operation_id: "op_2".into(),
                error: failed_error,
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Aborted {
                operation_id: "op_3".into(),
                reason: "cancelled".into(),
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Completed {
                operation_id: "op_direct_1".into(),
                turn_id: "turn_direct_1".into(),
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Failed {
                operation_id: "op_direct_2".into(),
                error: direct_failed_error,
            },
        );
        assert_prompt_event_eq(
            receiver.try_recv().unwrap(),
            PromptEvent::Aborted {
                operation_id: "op_direct_3".into(),
                reason: "stopped".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_agent_invocation_lifecycle_events() {
        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let failed_error = CodingSessionError::Provider {
            message: "child failed".into(),
        };

        service.emit_agent_invocation_started(
            "op_1",
            "child_op_1",
            ProfileId::from("coder"),
            "implement it",
        );
        service.emit_agent_invocation_completed(
            "op_1",
            "child_op_1",
            ProfileId::from("coder"),
            "done",
        );
        service.emit_agent_invocation_failed(
            "op_2",
            "child_op_2",
            ProfileId::from("coder"),
            failed_error.clone(),
        );
        service.emit_agent_invocation_aborted(
            "op_3",
            "child_op_3",
            ProfileId::from("coder"),
            "cancelled",
        );

        assert_agent_invocation_event_eq(
            receiver.try_recv().unwrap(),
            AgentInvocationEvent::Started {
                operation_id: "op_1".into(),
                child_operation_id: "child_op_1".into(),
                profile_id: ProfileId::from("coder"),
                task: "implement it".into(),
            },
        );
        assert_agent_invocation_event_eq(
            receiver.try_recv().unwrap(),
            AgentInvocationEvent::Completed {
                operation_id: "op_1".into(),
                child_operation_id: "child_op_1".into(),
                profile_id: ProfileId::from("coder"),
                final_text: "done".into(),
            },
        );
        assert_agent_invocation_event_eq(
            receiver.try_recv().unwrap(),
            AgentInvocationEvent::Failed {
                operation_id: "op_2".into(),
                child_operation_id: "child_op_2".into(),
                profile_id: ProfileId::from("coder"),
                error: failed_error,
            },
        );
        assert_agent_invocation_event_eq(
            receiver.try_recv().unwrap(),
            AgentInvocationEvent::Aborted {
                operation_id: "op_3".into(),
                child_operation_id: "child_op_3".into(),
                profile_id: ProfileId::from("coder"),
                reason: "cancelled".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_agent_team_lifecycle_events() {
        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let failed_error = CodingSessionError::Provider {
            message: "team failed".into(),
        };

        service.emit_agent_team_started("team_op_1", ProfileId::from("review-team"), "review it");
        service.emit_agent_team_member_started(
            "team_op_1",
            "child_op_1",
            ProfileId::from("review-team"),
            ProfileId::from("reviewer"),
            "review it",
        );
        service.emit_agent_team_member_completed(
            "team_op_1",
            "child_op_1",
            ProfileId::from("review-team"),
            ProfileId::from("reviewer"),
            "looks good",
        );
        service.emit_agent_team_completed("team_op_1", ProfileId::from("review-team"), "done");
        service.emit_agent_team_failed(
            "team_op_2",
            ProfileId::from("review-team"),
            failed_error.clone(),
        );
        service.emit_agent_team_aborted("team_op_3", ProfileId::from("review-team"), "cancelled");

        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::Started {
                operation_id: "team_op_1".into(),
                team_id: ProfileId::from("review-team"),
                task: "review it".into(),
            },
        );
        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::MemberStarted {
                operation_id: "team_op_1".into(),
                child_operation_id: "child_op_1".into(),
                team_id: ProfileId::from("review-team"),
                profile_id: ProfileId::from("reviewer"),
                task: "review it".into(),
            },
        );
        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::MemberCompleted {
                operation_id: "team_op_1".into(),
                child_operation_id: "child_op_1".into(),
                team_id: ProfileId::from("review-team"),
                profile_id: ProfileId::from("reviewer"),
                final_text: "looks good".into(),
            },
        );
        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::Completed {
                operation_id: "team_op_1".into(),
                team_id: ProfileId::from("review-team"),
                final_text: "done".into(),
            },
        );
        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::Failed {
                operation_id: "team_op_2".into(),
                team_id: ProfileId::from("review-team"),
                error: failed_error,
            },
        );
        assert_team_event_eq(
            receiver.try_recv().unwrap(),
            TeamEvent::Aborted {
                operation_id: "team_op_3".into(),
                team_id: ProfileId::from("review-team"),
                reason: "cancelled".into(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_delegation_lifecycle_events() {
        use crate::operations::prompt::context::DelegationRequest;

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let request = DelegationRequest {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "tool_1".into(),
            requesting_profile_id: ProfileId::from("planner"),
            target_kind: ProfileKind::Agent,
            target_id: ProfileId::from("coder"),
            task: "implement it".into(),
        };
        let failed_error = CodingSessionError::Provider {
            message: "child failed".into(),
        };

        let emitted = [
            service.emit_delegation_approved(&request),
            service.emit_delegation_confirmation_required(&request, "needs approval"),
            service.emit_delegation_rejected(&request, "not allowed"),
            service.emit_delegation_started(&request, "child_op_1"),
            service.emit_delegation_completed(&request, "child_op_1", "done"),
            service.emit_delegation_failed(&request, "child_op_2", failed_error),
        ];

        for expected in emitted {
            assert_eq!(receiver.try_recv().unwrap(), Some(expected));
        }
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_emits_self_healing_edit_events() {
        use crate::runtime::facade::{
            SelfHealingEditCheckOutput, SelfHealingEditDiagnostic, SelfHealingEditOutcome,
            SelfHealingEditRepairAttempt, SelfHealingEditReplacement,
        };

        let service = EventService::new();
        let mut receiver = service.subscribe_product_events();
        let check_output = SelfHealingEditCheckOutput {
            command: "cargo check".into(),
            stdout: "ok".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        let repair = SelfHealingEditRepairAttempt {
            attempt: 1,
            replacements: vec![SelfHealingEditReplacement::new("old", "new")],
            diagnostics: vec![SelfHealingEditDiagnostic {
                message: "fixed by repair".into(),
            }],
            check_output: Some(check_output.clone()),
        };
        let outcome = SelfHealingEditOutcome {
            path: "src/lib.rs".into(),
            message: "edited".into(),
            diff: String::new(),
            patch: String::new(),
            first_changed_line: Some(7),
            attempts: 2,
            diagnostics: Vec::new(),
            check_output: Some(check_output.clone()),
            repair_attempts: vec![repair.clone()],
        };
        let error = CodingSessionError::SelfHealingEditFailed {
            message: "check failed".into(),
            diagnostics: vec![SelfHealingEditDiagnostic {
                message: "missing symbol".into(),
            }],
            check_output: None,
            repair_attempts: Vec::new(),
        };

        service.emit_self_healing_edit_started("op_1", "src/lib.rs", 1);
        service.emit_self_healing_edit_repair_attempted("op_1", "src/lib.rs", &repair);
        service.emit_self_healing_edit_completed("op_1", &outcome);
        service.emit_self_healing_edit_failed("op_1", "src/lib.rs", &error);
        service.emit_self_healing_edit_aborted("op_2", "src/main.rs", "cancelled");

        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SelfHealingEditEvent::Started {
                operation_id: "op_1".into(),
                path: "src/lib.rs".into(),
                replacements: 1,
            }
            .into_product_draft(),
        );
        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SelfHealingEditEvent::RepairAttempted {
                operation_id: "op_1".into(),
                path: "src/lib.rs".into(),
                attempt: 1,
                replacements: vec![SelfHealingEditReplacement::new("old", "new")],
                diagnostics: vec![SelfHealingEditDiagnostic {
                    message: "fixed by repair".into(),
                }],
                check_output: Some(check_output.clone()),
            }
            .into_product_draft(),
        );
        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SelfHealingEditEvent::Completed {
                operation_id: "op_1".into(),
                path: "src/lib.rs".into(),
                attempts: 2,
                first_changed_line: Some(7),
                check_output: Some(check_output),
            }
            .into_product_draft(),
        );
        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SelfHealingEditEvent::Failed {
                operation_id: "op_1".into(),
                path: "src/lib.rs".into(),
                error,
            }
            .into_product_draft(),
        );
        assert_owner_draft_eq(
            receiver.try_recv().unwrap(),
            SelfHealingEditEvent::Aborted {
                operation_id: "op_2".into(),
                path: "src/main.rs".into(),
                reason: "cancelled".into(),
            }
            .into_product_draft(),
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn event_service_reports_bounded_product_event_window() {
        let service = EventService::with_event_capacity_for_tests(2);

        for index in 0..4 {
            service.emit_diagnostic(None::<String>, format!("event {index}"));
        }

        let status = service.backpressure_status();

        assert_eq!(status.channel_capacity, 2);
        assert_eq!(status.retained_capacity, 2);
        assert_eq!(
            status.oldest_retained_sequence,
            Some(ProductEventSequence::new(3))
        );
        assert_eq!(status.current_sequence, ProductEventSequence::new(4));
        assert_eq!(status.dropped_before, Some(ProductEventSequence::new(3)));
    }

    #[test]
    fn context_snapshot_survives_eviction_from_the_replay_window() {
        let coordinator = SnapshotCoordinator::new();
        coordinator.install_projection(
            crate::runtime::facade::CodingAgentSessionView {
                session_id: "session-1".into(),
                default_agent_profile_id: ProfileId::from("default"),
            },
            crate::runtime::facade::CodingAgentCapabilities::idle(false),
            crate::runtime::capability::CapabilityGeneration::new(1),
            0,
        );
        let service =
            EventService::with_event_capacity_and_coordinator_for_tests(2, coordinator.clone());

        service.emit_prompt_started("op-1", "turn-1");
        service.publish_prompt_stream_event(PromptStreamEvent::Tool(ToolEvent::Started {
            operation_id: "op-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: "tool-1".into(),
            name: "edit".into(),
            arguments_json: r#"{"path":"src/lib.rs","oldText":"a","newText":"b"}"#.into(),
        }));
        service.publish_prompt_stream_event(PromptStreamEvent::Tool(ToolEvent::Completed {
            operation_id: "op-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: "tool-1".into(),
            name: "edit".into(),
            summary: "updated".into(),
        }));
        for index in 0..4 {
            service.emit_diagnostic(None::<String>, format!("evict {index}"));
        }

        let status = service.backpressure_status();
        assert_eq!(status.retained_capacity, 2);
        assert!(status.oldest_retained_sequence.unwrap().get() > 3);
        let snapshot = coordinator.snapshot();
        assert_eq!(snapshot.context.changes.len(), 1);
        assert_eq!(snapshot.context.changes[0].path, "src/lib.rs");
        assert_eq!(snapshot.context.operations.len(), 1);
        assert_eq!(snapshot.context.operations[0].operation_id, "op-1");
        let public_snapshot: crate::runtime::facade::CodingAgentSnapshot = snapshot.into();
        assert_eq!(public_snapshot.context.changes[0].path, "src/lib.rs");
        assert_eq!(public_snapshot.context.operations[0].operation_id, "op-1");
    }

    #[tokio::test]
    async fn recovery_boundary_partitions_replay_and_live_events() {
        let service = EventService::with_event_capacity_for_tests(2);
        service.emit_diagnostic(None::<String>, "one");
        service.emit_diagnostic(None::<String>, "two");

        let ProductEventRecovery::Ready(mut boundary) =
            service.recovery_boundary_after(ProductEventSequence::new(1))
        else {
            panic!("cursor is within retained history");
        };
        assert_eq!(boundary.requested_after, ProductEventSequence::new(1));
        assert_eq!(boundary.replayed_through, ProductEventSequence::new(2));
        assert_eq!(
            boundary.oldest_available,
            Some(ProductEventSequence::new(1))
        );
        assert_eq!(
            boundary
                .replay
                .iter()
                .map(ProductEvent::sequence_internal)
                .collect::<Vec<_>>(),
            vec![ProductEventSequence::new(2)]
        );

        service.emit_diagnostic(None::<String>, "three");
        assert_eq!(
            boundary
                .receiver
                .try_recv()
                .unwrap()
                .unwrap()
                .sequence_internal(),
            ProductEventSequence::new(3)
        );
    }

    #[test]
    fn recovery_boundary_reports_retained_gap_but_accepts_initial_cursor() {
        let service = EventService::with_event_capacity_for_tests(2);
        for message in ["one", "two", "three"] {
            service.emit_diagnostic(None::<String>, message);
        }

        assert!(matches!(
            service.recovery_boundary_after(ProductEventSequence::new(1)),
            ProductEventRecovery::RetainedGap {
                requested_after: ProductEventSequence(1),
                oldest_available: ProductEventSequence(2),
            }
        ));
        assert!(matches!(
            service.recovery_boundary_after(ProductEventSequence::default()),
            ProductEventRecovery::Ready(_)
        ));
    }

    #[test]
    fn pressure_classes_never_replay_across_a_sequence_gap() {
        use crate::events::CodingAgentProductEventDeliveryClass;

        let service = EventService::with_event_capacity_for_tests(2);
        service.emit_diagnostic(None::<String>, "evicted data");
        service.emit_capability_changed(installed_capability(
            2,
            crate::runtime::capability::CapabilityRevocationPolicy::FutureOnly,
            Vec::new(),
        ));
        service.emit_operation_recovered(
            "op_recovered",
            "recovery_1",
            "startup recovery",
            "session_recovered",
            Some(crate::runtime::control::OperationKind::Prompt),
            Some(1),
        );

        assert!(matches!(
            service.recovery_boundary_after(ProductEventSequence::new(1)),
            ProductEventRecovery::RetainedGap { .. }
        ));
        let ProductEventRecovery::Ready(boundary) =
            service.recovery_boundary_after(ProductEventSequence::default())
        else {
            panic!("initial subscription may start from the retained boundary")
        };
        assert_eq!(
            boundary
                .replay
                .iter()
                .map(ProductEvent::delivery_class)
                .collect::<Vec<_>>(),
            vec![
                CodingAgentProductEventDeliveryClass::Control,
                CodingAgentProductEventDeliveryClass::Recovery,
            ]
        );
    }

    #[tokio::test]
    async fn product_event_receiver_lag_reports_snapshot_recovery() {
        let service = EventService::with_event_capacity_for_tests(1);
        let mut receiver = service.subscribe_product_events();

        for index in 0..3 {
            service.emit_diagnostic(None::<String>, format!("event {index}"));
        }

        let error = receiver.recv().await.unwrap_err();

        assert_eq!(error.code(), "event_stream_lag");
        assert!(
            error
                .to_string()
                .contains("client must request a fresh UI snapshot")
        );
    }

    #[test]
    fn recovery_markers_publish_terminal_product_events() {
        let service = EventService::new();
        let event = service.emit_operation_recovered(
            "op_recovered",
            "recovery_1",
            "startup recovery marked incomplete operation in-doubt",
            "session_recovered",
            Some(crate::runtime::control::OperationKind::Prompt),
            Some(7),
        );

        assert_eq!(event.operation_id(), Some("op_recovered"));
        assert_eq!(event.capability_generation(), Some(7));
        assert_eq!(event.root_operation_id(), Some("op_recovered"));
        assert_eq!(event.session_id(), Some("session_recovered"));
        assert_eq!(
            event.terminal_operation().unwrap().kind,
            crate::events::CodingAgentProductEventTerminalOperationKind::Prompt
        );
        assert_eq!(
            event.durability(),
            &crate::events::CodingAgentProductEventDurability::DerivedFromSession {
                session_id: "session_recovered".into(),
                source_operation_id: "op_recovered".into(),
                recovery_id: "recovery_1".into(),
            }
        );
        assert_eq!(
            event.delivery_class(),
            crate::events::CodingAgentProductEventDeliveryClass::Recovery
        );
        assert_eq!(
            event.terminal_status(),
            Some(ProductEventTerminalStatus::Recovered)
        );
        assert_eq!(
            event.event().family(),
            CodingAgentProductEventFamily::Workflow
        );
    }

    #[test]
    fn durable_outbox_redelivery_is_idempotent_per_runtime() {
        let service = EventService::new();
        let draft =
            EventService::session_write_committed_event("op_redelivery", "session_redelivery")
                .into_product_draft();
        let record = crate::events::outbox::DurableOutboxRecord::new(
            "session_redelivery/op_redelivery/session_write_committed",
            "session_redelivery",
            Some("op_redelivery".into()),
            vec!["evt_redelivery".into()],
            crate::events::outbox::DurableOutboxRecordKind::SessionWrite,
            draft,
            3,
        )
        .unwrap();

        assert!(service.emit_durable_outbox_record(&record).is_some());
        assert!(service.emit_durable_outbox_record(&record).is_none());
        assert_eq!(service.current_product_sequence().get(), 1);
    }

    #[test]
    fn durable_outbox_redelivery_matrix_preserves_order_and_gap_recovery() {
        let service = EventService::with_event_capacity_for_tests(2);
        let records = (1..=3)
            .map(|index| {
                let operation_id = format!("op_matrix_{index}");
                let session_id = "session_matrix";
                let draft =
                    EventService::session_write_committed_event(operation_id.clone(), session_id)
                        .into_product_draft();
                crate::events::outbox::DurableOutboxRecord::new(
                    format!("{session_id}/{operation_id}/session_write_committed"),
                    session_id,
                    Some(operation_id),
                    vec![format!("evt_matrix_{index}")],
                    crate::events::outbox::DurableOutboxRecordKind::SessionWrite,
                    draft,
                    index,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        for record in &records {
            assert!(service.emit_durable_outbox_record(record).is_some());
        }
        assert!(service.emit_durable_outbox_record(&records[0]).is_none());
        assert_eq!(service.current_product_sequence().get(), 3);

        let ProductEventRecovery::Ready(boundary) =
            service.recovery_boundary_after(ProductEventSequence::default())
        else {
            panic!("initial cursor should replay retained events");
        };
        assert_eq!(boundary.replayed_through.get(), 3);
        assert_eq!(
            boundary
                .replay
                .iter()
                .map(ProductEvent::sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(matches!(
            service.recovery_boundary_after(ProductEventSequence::new(1)),
            ProductEventRecovery::RetainedGap {
                requested_after,
                oldest_available
            } if requested_after.get() == 1 && oldest_available.get() == 2
        ));
        let ProductEventRecovery::Ready(boundary) =
            service.recovery_boundary_after(ProductEventSequence::new(3))
        else {
            panic!("current cursor should reconnect without replay");
        };
        assert!(boundary.replay.is_empty());
    }
}
