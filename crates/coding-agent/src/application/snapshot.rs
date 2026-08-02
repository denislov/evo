use super::operation::control::{OperationCancellationHandle, OperationKind, PromptControlHandle};
use crate::events::{ProductEvent, ProductEventSequence, ProductEventTerminalStatus};
use crate::kernel::capability::CapabilityGeneration;
use crate::kernel::error::{CodingAgentLifecycleRejection, CodingSessionError};
use crate::kernel::operation::OperationDescriptor;
use crate::mutex::MutexExt;
use crate::runtime::client::context::UiContextProjection;
use crate::runtime::client::state::{
    ClientConnectionId, ClientDraft, UiSnapshot, UiSnapshotCursor,
};
use crate::runtime::facade::context::CodingAgentCapabilities;
use crate::runtime::version::UI_SNAPSHOT_PROTOCOL_VERSION;
use crate::session::view::CodingAgentSessionView;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub(crate) const MAX_CLIENTS: usize = 64;
pub(crate) const MAX_DRAFTS: usize = 64;
pub(crate) const MAX_RECEIPTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ClientGeneration(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientHandle {
    pub(crate) id: ClientConnectionId,
    pub(crate) generation: ClientGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmittedEventDurability {
    Durable,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubmittedTerminalAnchor {
    ProductEvent {
        sequence: u64,
        durability: SubmittedEventDurability,
    },
    OutcomeOnly {
        acknowledgement: crate::runtime::client::connection::CodingAgentOutcomeAcknowledgementId,
    },
    TerminalUncertain {
        operation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubmittedOperationStatus {
    Running {
        operation_id: String,
        kind: OperationKind,
        descriptor: OperationDescriptor,
    },
    RecoveryPending {
        operation_id: String,
        kind: OperationKind,
        descriptor: OperationDescriptor,
        recovery_id: String,
    },
    Terminal {
        operation_id: String,
        kind: OperationKind,
        descriptor: OperationDescriptor,
        anchor: SubmittedTerminalAnchor,
        status: ProductEventTerminalStatus,
        root_count: u8,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ClientSnapshotState {
    pub(crate) snapshot: UiSnapshot,
    pub(crate) drafts: Vec<DraftRecord>,
    pub(crate) submitted_operation: Option<SubmittedOperationStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftRecord {
    pub(crate) id: String,
    pub(crate) kind: crate::runtime::client::state::ClientDraftKind,
    pub(crate) text: String,
    pub(crate) fingerprint: String,
}

#[derive(Debug, Clone)]
enum PromptControlPayload {
    Text(String),
    Content(Vec<ai::api::conversation::ContentBlock>),
}

impl PromptControlPayload {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Content(content) => content.is_empty(),
        }
    }

    fn signature(&self) -> String {
        match self {
            Self::Text(text) => format!("text:{text}"),
            Self::Content(content) => format!(
                "content:{}",
                serde_json::to_string(content)
                    .expect("prompt control content blocks must serialize")
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientRecord {
    pub(crate) generation: ClientGeneration,
    connection: ConnectionLifecycle,
    pub(crate) acknowledged_sequence: u64,
    pub(crate) prompt_draft: Option<DraftRecord>,
    pub(crate) steer_drafts: VecDeque<DraftRecord>,
    pub(crate) follow_up_drafts: VecDeque<DraftRecord>,
    prepared_operation: Option<PreparedOperation>,
    pending_abort_operation_id: Option<String>,
    pub(crate) submitted_operation: Option<SubmittedOperationStatus>,
    pub(crate) control_receipts: HashMap<String, String>,
}

impl ClientRecord {
    fn new(generation: ClientGeneration) -> Self {
        Self {
            generation,
            connection: ConnectionLifecycle::Attached,
            acknowledged_sequence: 0,
            prompt_draft: None,
            steer_drafts: VecDeque::new(),
            follow_up_drafts: VecDeque::new(),
            prepared_operation: None,
            pending_abort_operation_id: None,
            submitted_operation: None,
            control_receipts: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedOperation {
    operation_id: String,
    descriptor: OperationDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionLifecycle {
    Attached,
    ShuttingDown,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeLifecycle {
    Running,
    ShuttingDown,
    ShutDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientDetachOutcome {
    Detached,
    AlreadyDetached,
    StaleGeneration,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SnapshotProjection {
    pub(crate) revision: u64,
    pub(crate) session: CodingAgentSessionView,
    pub(crate) capabilities: CodingAgentCapabilities,
    pub(crate) active_operation: Option<OperationKind>,
    pub(crate) capability_generation: CapabilityGeneration,
}

#[derive(Debug)]
pub(crate) struct SnapshotState {
    pub(crate) runtime_lifecycle: RuntimeLifecycle,
    pub(crate) lifecycle_epoch: u64,
    pub(crate) clients: HashMap<ClientConnectionId, ClientRecord>,
    pub(crate) projection: Option<SnapshotProjection>,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) event_stream_id: String,
    pub(crate) operation_event_contexts: HashMap<String, OperationEventContext>,
    pub(crate) next_event_sequence: u64,
    pub(crate) committed_session_sequence: u64,
    pub(crate) retained_product_events: VecDeque<ProductEvent>,
    pub(crate) dropped_before: Option<ProductEventSequence>,
    pub(crate) recovery_revision: u64,
    pub(crate) shutdown_drain_boundary: Option<ProductEventSequence>,
    pub(crate) pending_authorizations: Vec<crate::authorization::ToolAuthorizationRequest>,
    pub(crate) published_outbox_record_ids: HashSet<String>,
    pub(crate) context_projection: UiContextProjection,
    shutdown_drain_eligibility: HashMap<ClientConnectionId, ClientGeneration>,
}

impl Default for SnapshotState {
    fn default() -> Self {
        Self {
            runtime_lifecycle: RuntimeLifecycle::Running,
            lifecycle_epoch: 0,
            clients: HashMap::new(),
            projection: None,
            capability_generation: CapabilityGeneration::new(1),
            event_stream_id: crate::platform::time::new_product_event_stream_id(),
            operation_event_contexts: HashMap::new(),
            next_event_sequence: 1,
            committed_session_sequence: 0,
            retained_product_events: VecDeque::new(),
            dropped_before: None,
            recovery_revision: 0,
            shutdown_drain_boundary: None,
            pending_authorizations: Vec::new(),
            published_outbox_record_ids: HashSet::new(),
            context_projection: UiContextProjection::default(),
            shutdown_drain_eligibility: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationEventContext {
    pub(crate) kind: OperationKind,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) parent_operation_id: Option<String>,
    pub(crate) root_operation_id: String,
}

#[derive(Debug)]
pub(crate) struct SnapshotCoordinator {
    pub(crate) state: Mutex<SnapshotState>,
    capability_transition: Mutex<()>,
    prompt_control: Mutex<Option<PromptControlBinding>>,
    operation_cancellations: Mutex<HashMap<String, OperationCancellationBinding>>,
    lifecycle_sender: watch::Sender<u64>,
    #[cfg(test)]
    submission_transition_probe: Mutex<Option<SubmissionTransitionProbe>>,
}

#[derive(Debug, Clone)]
struct PromptControlBinding {
    owner: ClientHandle,
    operation_id: String,
    channel_generation: super::operation::control::PromptControlGeneration,
    sender: PromptControlHandle,
}

#[derive(Debug, Clone)]
struct OperationCancellationBinding {
    owner: ClientHandle,
    cancellation: OperationCancellationHandle,
}

#[cfg(test)]
#[derive(Debug)]
struct SubmissionTransitionProbe {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Default for SnapshotCoordinator {
    fn default() -> Self {
        let (lifecycle_sender, _) = watch::channel(0);
        Self {
            state: Mutex::new(SnapshotState::default()),
            capability_transition: Mutex::new(()),
            prompt_control: Mutex::new(None),
            operation_cancellations: Mutex::new(HashMap::new()),
            lifecycle_sender,
            #[cfg(test)]
            submission_transition_probe: Mutex::new(None),
        }
    }
}

mod capability_state;
mod client_registry;
mod lifecycle;
mod projection;

fn control_rejection_reason(
    error: &ClientRegistryError,
) -> crate::runtime::client::connection::CodingAgentControlRejectionReason {
    use crate::runtime::client::connection::CodingAgentControlRejectionReason;
    match error {
        ClientRegistryError::Lifecycle(CodingAgentLifecycleRejection::Detached) => {
            CodingAgentControlRejectionReason::Detached
        }
        ClientRegistryError::Lifecycle(CodingAgentLifecycleRejection::StaleGeneration) => {
            CodingAgentControlRejectionReason::StaleGeneration
        }
        ClientRegistryError::Lifecycle(CodingAgentLifecycleRejection::RuntimeShutDown) => {
            CodingAgentControlRejectionReason::RuntimeShutDown
        }
        _ => CodingAgentControlRejectionReason::InvalidInput,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ClientRegistryError {
    #[error("client registry resource error: {message}")]
    Resource { message: String },
    #[error("lifecycle rejection: {0}")]
    Lifecycle(CodingAgentLifecycleRejection),
    #[error("client capacity exceeded: {limit}")]
    ClientCapacityExceeded { limit: usize },
    #[error("draft queue capacity exceeded: {limit}")]
    QueueCapacityExceeded { limit: usize },
    #[error("invalid client input")]
    InvalidInput,
    #[error("submitted operation transition regressed")]
    SubmittedRegression,
    #[error("client already has a submitted operation")]
    SubmittedOperationPending,
    #[error("prepared submission draft no longer matches")]
    SubmissionDraftMismatch,
    #[error("submitted terminal root cardinality was {count}, expected exactly one")]
    TerminalCardinality { count: u8 },
}

impl From<CodingSessionError> for ClientRegistryError {
    fn from(error: CodingSessionError) -> Self {
        Self::Resource {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod submission_escape_hatch_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;
    use crate::application::operation::contract::CodingAgentOperation;
    use crate::events::CodingAgentProductEventTerminalStatus;
    use crate::runtime::client::state::ClientConnectionId;

    fn coordinator() -> SnapshotCoordinator {
        SnapshotCoordinator::default()
    }

    fn descriptor() -> OperationDescriptor {
        CodingAgentOperation::SetSessionName { name: None }.descriptor()
    }

    fn submitted_operation(
        coordinator: &SnapshotCoordinator,
        handle: &ClientHandle,
    ) -> Option<SubmittedOperationStatus> {
        coordinator
            .state
            .lock_or_recover("test runtime snapshot state")
            .clients
            .get(&handle.id)
            .and_then(|record| record.submitted_operation.clone())
    }

    #[test]
    fn runtime_lifecycle_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum Action {
            RequestShutdown,
            FinishShutdown,
        }

        let cases = [
            (
                "running accepts shutdown request",
                RuntimeLifecycle::Running,
                Action::RequestShutdown,
                RuntimeLifecycle::Running,
                RuntimeLifecycle::ShuttingDown,
            ),
            (
                "shutdown request is idempotent while draining",
                RuntimeLifecycle::ShuttingDown,
                Action::RequestShutdown,
                RuntimeLifecycle::ShuttingDown,
                RuntimeLifecycle::ShuttingDown,
            ),
            (
                "shutdown request is idempotent after completion",
                RuntimeLifecycle::ShutDown,
                Action::RequestShutdown,
                RuntimeLifecycle::ShutDown,
                RuntimeLifecycle::ShutDown,
            ),
            (
                "finish closes a draining runtime",
                RuntimeLifecycle::ShuttingDown,
                Action::FinishShutdown,
                RuntimeLifecycle::ShuttingDown,
                RuntimeLifecycle::ShutDown,
            ),
            (
                "finish is idempotent after completion",
                RuntimeLifecycle::ShutDown,
                Action::FinishShutdown,
                RuntimeLifecycle::ShutDown,
                RuntimeLifecycle::ShutDown,
            ),
        ];

        for (name, initial, action, expected_return, expected_state) in cases {
            let coordinator = coordinator();
            coordinator
                .state
                .lock_or_recover("test runtime snapshot state")
                .runtime_lifecycle = initial;
            let observed_return = match action {
                Action::RequestShutdown => coordinator.request_shutdown().unwrap(),
                Action::FinishShutdown => {
                    coordinator.finish_shutdown().unwrap();
                    initial
                }
            };
            let observed_state = coordinator
                .state
                .lock_or_recover("test runtime snapshot state")
                .runtime_lifecycle;
            assert_eq!(observed_return, expected_return, "{name}");
            assert_eq!(observed_state, expected_state, "{name}");
        }
    }

    #[test]
    fn client_receiver_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum RecordState {
            Missing,
            Attached(ClientGeneration),
            ShuttingDown(ClientGeneration),
            Detached(ClientGeneration),
        }

        let cases = [
            (
                "current attached receiver",
                RuntimeLifecycle::Running,
                RecordState::Attached(ClientGeneration(1)),
                None,
            ),
            (
                "current receiver drains during shutdown",
                RuntimeLifecycle::Running,
                RecordState::ShuttingDown(ClientGeneration(1)),
                None,
            ),
            (
                "detached receiver",
                RuntimeLifecycle::Running,
                RecordState::Detached(ClientGeneration(1)),
                Some(CodingAgentLifecycleRejection::Detached),
            ),
            (
                "missing receiver",
                RuntimeLifecycle::Running,
                RecordState::Missing,
                Some(CodingAgentLifecycleRejection::StaleGeneration),
            ),
            (
                "superseded receiver generation",
                RuntimeLifecycle::Running,
                RecordState::Attached(ClientGeneration(2)),
                Some(CodingAgentLifecycleRejection::StaleGeneration),
            ),
            (
                "receiver after runtime shutdown",
                RuntimeLifecycle::ShutDown,
                RecordState::Attached(ClientGeneration(1)),
                Some(CodingAgentLifecycleRejection::RuntimeShutDown),
            ),
        ];

        for (name, runtime_lifecycle, record_state, expected_error) in cases {
            let client_id = ClientConnectionId::new("client-transition");
            let handle = ClientHandle {
                id: client_id.clone(),
                generation: ClientGeneration(1),
            };
            let mut state = SnapshotState {
                runtime_lifecycle,
                ..SnapshotState::default()
            };
            let record = match record_state {
                RecordState::Missing => None,
                RecordState::Attached(generation) => {
                    Some((generation, ConnectionLifecycle::Attached))
                }
                RecordState::ShuttingDown(generation) => {
                    Some((generation, ConnectionLifecycle::ShuttingDown))
                }
                RecordState::Detached(generation) => {
                    Some((generation, ConnectionLifecycle::Detached))
                }
            };
            if let Some((generation, connection)) = record {
                let mut record = ClientRecord::new(generation);
                record.connection = connection;
                state.clients.insert(client_id, record);
            }

            let observed_error =
                SnapshotCoordinator::validate_receiver_in_state(&state, &handle, None)
                    .err()
                    .map(|error| match error {
                        ClientRegistryError::Lifecycle(reason) => reason,
                        other => panic!("{name}: unexpected registry error: {other}"),
                    });
            assert_eq!(observed_error, expected_error, "{name}");
        }
    }

    #[test]
    fn submission_slot_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum SlotState {
            Empty,
            Prepared,
            Running,
            RecoveryPending,
            Terminal,
        }

        let cases = [
            ("empty slot", SlotState::Empty, true),
            ("prepared operation", SlotState::Prepared, false),
            ("running operation", SlotState::Running, false),
            (
                "recovery-pending operation",
                SlotState::RecoveryPending,
                false,
            ),
            ("terminal operation", SlotState::Terminal, true),
        ];

        for (name, slot_state, expected_available) in cases {
            let coordinator = coordinator();
            let handle = coordinator
                .connect_or_takeover(ClientConnectionId::new(format!("client-{name}")))
                .unwrap();
            {
                let mut state = coordinator
                    .state
                    .lock_or_recover("test runtime snapshot state");
                let record = state.clients.get_mut(&handle.id).unwrap();
                match slot_state {
                    SlotState::Empty => {}
                    SlotState::Prepared => {
                        record.prepared_operation = Some(PreparedOperation {
                            operation_id: "operation".into(),
                            descriptor: descriptor(),
                        });
                    }
                    SlotState::Running => {
                        let descriptor = descriptor();
                        record.submitted_operation = Some(SubmittedOperationStatus::Running {
                            operation_id: "operation".into(),
                            kind: descriptor.submitted_kind,
                            descriptor,
                        });
                    }
                    SlotState::RecoveryPending => {
                        let descriptor = descriptor();
                        record.submitted_operation =
                            Some(SubmittedOperationStatus::RecoveryPending {
                                operation_id: "operation".into(),
                                kind: descriptor.submitted_kind,
                                descriptor,
                                recovery_id: "recovery".into(),
                            });
                    }
                    SlotState::Terminal => {
                        let descriptor = descriptor();
                        record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
                            operation_id: "operation".into(),
                            kind: descriptor.submitted_kind,
                            descriptor,
                            anchor: SubmittedTerminalAnchor::TerminalUncertain {
                                operation_id: "operation".into(),
                            },
                            status: ProductEventTerminalStatus::Failed,
                            root_count: 0,
                        });
                    }
                }
            }

            assert_eq!(
                coordinator.validate_submission_slot(&handle).is_ok(),
                expected_available,
                "{name}"
            );
        }
    }

    #[test]
    fn poisoned_snapshot_state_degrades_to_resource_error() {
        let coordinator = coordinator();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _state = coordinator
                .state
                .lock_or_recover("test runtime snapshot state");
            panic!("poison runtime snapshot state");
        }));

        let error = coordinator.ensure_runtime_running().unwrap_err();
        assert!(matches!(error, CodingSessionError::Resource { .. }));
        assert!(error.to_string().contains("runtime snapshot state"));
    }

    #[test]
    fn terminal_uncertain_submission_can_be_replaced_by_a_fresh_submission() {
        let coordinator = coordinator();
        let handle = coordinator
            .connect_or_takeover(ClientConnectionId::new("client-1"))
            .unwrap();

        // Simulate a failed prompt: the operation ran, never produced a
        // terminal product event, and finalization degraded the Running
        // submission into TerminalUncertain.
        let first = "op-1".to_owned();
        coordinator
            .register_prepared_submission(&handle, first.clone(), descriptor())
            .unwrap();
        coordinator
            .commit_submission_running(&handle, first.clone(), descriptor(), None)
            .unwrap();
        coordinator
            .finalize_terminal_association(
                &handle,
                &first,
                descriptor(),
                CodingAgentProductEventTerminalStatus::Failed,
            )
            .unwrap();
        assert!(
            matches!(
                submitted_operation(&coordinator, &handle),
                Some(SubmittedOperationStatus::Terminal {
                    anchor: SubmittedTerminalAnchor::TerminalUncertain { .. },
                    ..
                })
            ),
            "failed finalization must degrade the submission to TerminalUncertain"
        );

        // The escape hatch: the stale terminal submission must not block a
        // fresh prepare/commit, and the new submission must replace it.
        assert!(coordinator.validate_submission_slot(&handle).is_ok());
        let second = "op-2".to_owned();
        coordinator
            .register_prepared_submission(&handle, second.clone(), descriptor())
            .unwrap();
        coordinator
            .commit_submission_running(&handle, second.clone(), descriptor(), None)
            .unwrap();
        assert!(
            matches!(
                submitted_operation(&coordinator, &handle),
                Some(SubmittedOperationStatus::Running { .. })
            ),
            "the fresh submission must replace the stale terminal one"
        );
    }

    #[test]
    fn running_or_recovery_pending_submissions_still_block_prepare() {
        let coordinator = coordinator();
        let handle = coordinator
            .connect_or_takeover(ClientConnectionId::new("client-2"))
            .unwrap();

        let operation_id = "op-1".to_owned();
        coordinator
            .register_prepared_submission(&handle, operation_id.clone(), descriptor())
            .unwrap();
        coordinator
            .commit_submission_running(&handle, operation_id.clone(), descriptor(), None)
            .unwrap();

        assert!(
            matches!(
                coordinator.validate_submission_slot(&handle),
                Err(ClientRegistryError::SubmittedOperationPending)
            ),
            "a running submission must still block the slot"
        );
        assert!(
            coordinator
                .register_prepared_submission(&handle, "op-2".to_owned(), descriptor())
                .is_err(),
            "a running submission must still block prepare"
        );
    }
}
