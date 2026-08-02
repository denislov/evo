use futures::future::{BoxFuture, FutureExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use crate::application::operation::finalize::{FinalizationCommitResult, FinalizationDecision};
use crate::application::snapshot::{ClientHandle, ClientRegistryError, SnapshotCoordinator};
use crate::events::agent::AgentInvocationEvent;
use crate::events::capability::CapabilityEvent;
use crate::events::delegation::{DelegationEvent, DelegationEventContext};
use crate::events::diagnostic::DiagnosticEvent;
use crate::events::emission::ProductEventDraft;
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
use crate::events::{CodingAgentSessionWriteFailureReason, CodingAgentSessionWriteFailureStatus};
use crate::kernel::capability::CapabilityGeneration;
use crate::kernel::capability::InstalledCapabilityGeneration;
use crate::kernel::error::CodingSessionError;
use crate::kernel::ids::ProfileId;
use crate::kernel::operation::{OperationKind, OperationRootTerminalEvidence};
use crate::mutex::MutexExt;
use crate::mutex::report_infallible_resource_error;
use crate::operations::prompt::context::{DelegationRequest, InternalPromptTurnOutcome};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditObserver, SelfHealingEditOutcome, SelfHealingEditRepairAttempt,
};
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
    capability_generation: Option<CapabilityGeneration>,
    operation_kind: Option<OperationKind>,
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
            report_infallible_resource_error(
                "self-healing edit observer event",
                self.event_service.emit_self_healing_edit_repair_attempted(
                    self.operation_id.clone(),
                    path,
                    repair,
                ),
            );
        }
        .boxed()
    }
}

mod durable;
mod emit;
mod publish;

#[cfg(test)]
mod transition_table_tests {
    use super::*;

    fn service(retained_capacity: usize) -> (EventService, Arc<SnapshotCoordinator>) {
        let coordinator = SnapshotCoordinator::new();
        let service = EventService::with_event_capacities_and_coordinator(
            8,
            retained_capacity,
            coordinator.clone(),
        );
        (service, coordinator)
    }

    #[test]
    fn retained_publication_transition_table() {
        let cases: &[(usize, &[u64], Option<u64>)] = &[
            (0, &[], Some(4)),
            (1, &[3], Some(3)),
            (2, &[2, 3], Some(2)),
            (4, &[1, 2, 3], None),
        ];

        for (retained_capacity, expected_sequences, expected_dropped_before) in cases {
            let (service, coordinator) = service(*retained_capacity);
            for sequence in 1..=3 {
                let event = service
                    .emit_session_opened(format!("session-{sequence}"))
                    .unwrap();
                assert_eq!(event.sequence(), sequence);
            }
            let state = coordinator
                .state
                .lock_resource("test runtime snapshot state")
                .unwrap();
            let retained = state
                .retained_product_events
                .iter()
                .map(ProductEvent::sequence)
                .collect::<Vec<_>>();
            assert_eq!(
                retained, *expected_sequences,
                "capacity {retained_capacity}"
            );
            assert_eq!(
                state.dropped_before.map(ProductEventSequence::get),
                *expected_dropped_before,
                "capacity {retained_capacity}"
            );
            assert_eq!(state.next_event_sequence, 4, "capacity {retained_capacity}");
        }
    }

    #[test]
    fn recovery_boundary_transition_table() {
        #[derive(Debug)]
        enum Expected<'a> {
            Ready(&'a [u64]),
            Gap { oldest_available: u64 },
        }

        let (service, coordinator) = service(2);
        for sequence in 1..=3 {
            service
                .emit_session_opened(format!("session-{sequence}"))
                .unwrap();
        }
        let cases = [
            (0, Expected::Ready(&[2, 3])),
            (
                1,
                Expected::Gap {
                    oldest_available: 2,
                },
            ),
            (2, Expected::Ready(&[3])),
            (3, Expected::Ready(&[])),
        ];

        for (cursor, expected) in cases {
            let state = coordinator
                .state
                .lock_resource("test runtime snapshot state")
                .unwrap();
            let recovery =
                service.recovery_boundary_from_state(&state, ProductEventSequence::new(cursor));
            drop(state);
            match (recovery, expected) {
                (ProductEventRecovery::Ready(boundary), Expected::Ready(expected_sequences)) => {
                    assert_eq!(boundary.replayed_through.get(), 3, "cursor {cursor}");
                    assert_eq!(
                        boundary
                            .replay
                            .iter()
                            .map(ProductEvent::sequence)
                            .collect::<Vec<_>>(),
                        expected_sequences,
                        "cursor {cursor}"
                    );
                }
                (
                    ProductEventRecovery::RetainedGap {
                        requested_after,
                        oldest_available,
                    },
                    Expected::Gap {
                        oldest_available: expected_oldest,
                    },
                ) => {
                    assert_eq!(requested_after.get(), cursor);
                    assert_eq!(oldest_available.get(), expected_oldest);
                }
                (actual, expected) => {
                    panic!("cursor {cursor}: expected {expected:?}, got {actual:?}")
                }
            }
        }
    }

    #[test]
    fn deferred_terminal_draft_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum Action {
            Observe,
            Defer(&'static str),
            Take,
        }

        let (service, _) = service(4);
        let cases = [
            (Action::Observe, false, None),
            (Action::Defer("first"), true, None),
            (Action::Defer("replacement"), true, None),
            (Action::Take, false, Some("replacement")),
            (Action::Take, false, None),
        ];

        for (action, expected_present, expected_taken_reason) in cases {
            let taken_reason = match action {
                Action::Observe => None,
                Action::Defer(reason) => {
                    let draft = EventService::session_write_skipped_event("operation", reason)
                        .into_product_draft();
                    service.defer_terminal_draft("operation", draft).unwrap();
                    None
                }
                Action::Take => service
                    .take_deferred_terminal_draft("operation")
                    .unwrap()
                    .map(|draft| match draft.event {
                        CodingAgentProductEventKind::Session(
                            crate::events::CodingAgentSessionProductEvent::WriteSkipped {
                                reason,
                                ..
                            },
                        ) => reason,
                        event => panic!("unexpected deferred event: {event:?}"),
                    }),
            };
            assert_eq!(
                service.has_deferred_terminal_draft("operation").unwrap(),
                expected_present,
                "{action:?}"
            );
            assert_eq!(taken_reason.as_deref(), expected_taken_reason, "{action:?}");
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

pub(crate) use crate::domain::projection::agent::{AgentEventMappingContext, map_agent_event};
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
                    if self.snapshot_coordinator.is_shut_down()? {
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
                if self.snapshot_coordinator.is_shut_down()? {
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
