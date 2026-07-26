use super::capability::CapabilityGeneration;
use super::control::{OperationCancellationHandle, OperationKind, PromptControlHandle};
use crate::events::{ProductEvent, ProductEventSequence, ProductEventTerminalStatus};
use crate::runtime::client::context::UiContextProjection;
use crate::runtime::client::state::{
    ClientConnectionId, ClientDraft, UiSnapshot, UiSnapshotCursor,
};
use crate::runtime::error::CodingAgentLifecycleRejection;
use crate::runtime::facade::CodingSessionError;
use crate::runtime::facade::context::{CodingAgentCapabilities, CodingAgentSessionView};
use crate::runtime::outcome::OperationDescriptor;
use crate::runtime::version::UI_SNAPSHOT_PROTOCOL_VERSION;
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
        acknowledgement: crate::runtime::client::projection::CodingAgentOutcomeAcknowledgementId,
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
    pub(crate) control_receipt_order: VecDeque<String>,
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
            control_receipt_order: VecDeque::new(),
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
            event_stream_id: crate::session::id::new_product_event_stream_id(),
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
    channel_generation: super::control::PromptControlGeneration,
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

impl SnapshotCoordinator {
    pub(crate) fn register_operation_event_context(
        &self,
        operation_id: String,
        kind: OperationKind,
        generation: CapabilityGeneration,
        parent_operation_id: Option<String>,
        root_operation_id: String,
    ) {
        let mut state = self.state.lock().unwrap();
        let context = OperationEventContext {
            kind,
            capability_generation: generation,
            parent_operation_id,
            root_operation_id,
        };
        let previous = state
            .operation_event_contexts
            .insert(operation_id, context.clone());
        debug_assert!(previous.is_none() || previous == Some(context));
    }

    pub(crate) fn clear_operation_event_context_if(
        &self,
        operation_id: &str,
        generation: CapabilityGeneration,
    ) {
        let mut state = self.state.lock().unwrap();
        if state
            .operation_event_contexts
            .get(operation_id)
            .is_some_and(|current| current.capability_generation == generation)
        {
            state.operation_event_contexts.remove(operation_id);
        }
    }

    pub(crate) fn ensure_runtime_running(&self) -> Result<(), CodingSessionError> {
        let state = self.state.lock().unwrap();
        Self::validate_runtime(&state).map_err(|error| match error {
            ClientRegistryError::Lifecycle(reason) => CodingSessionError::Lifecycle { reason },
            other => CodingSessionError::Input {
                message: other.to_string(),
            },
        })
    }

    pub(crate) fn request_shutdown(&self) -> RuntimeLifecycle {
        let mut state = self.state.lock().unwrap();
        let previous = state.runtime_lifecycle;
        if previous != RuntimeLifecycle::Running {
            return previous;
        }
        state.runtime_lifecycle = RuntimeLifecycle::ShuttingDown;
        state.shutdown_drain_eligibility.clear();
        let mut eligible = Vec::new();
        for (id, record) in &mut state.clients {
            if record.connection == ConnectionLifecycle::Attached {
                record.connection = ConnectionLifecycle::ShuttingDown;
                eligible.push((id.clone(), record.generation));
            }
        }
        state.shutdown_drain_eligibility.extend(eligible);
        state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
        let lifecycle_epoch = state.lifecycle_epoch;
        drop(state);
        *self.prompt_control.lock().unwrap() = None;
        self.lifecycle_sender.send_replace(lifecycle_epoch);
        previous
    }

    pub(crate) async fn wait_for_active_operation_to_drain(&self) {
        let mut receiver = self.subscribe_lifecycle();
        loop {
            let active = self
                .state
                .lock()
                .unwrap()
                .projection
                .as_ref()
                .and_then(|projection| projection.active_operation);
            if active.is_none() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) fn finish_shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        if state.runtime_lifecycle == RuntimeLifecycle::ShutDown {
            return;
        }
        debug_assert_eq!(state.runtime_lifecycle, RuntimeLifecycle::ShuttingDown);
        state.runtime_lifecycle = RuntimeLifecycle::ShutDown;
        for record in state.clients.values_mut() {
            if record.connection == ConnectionLifecycle::ShuttingDown {
                record.connection = ConnectionLifecycle::Detached;
            }
        }
        state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
        let lifecycle_epoch = state.lifecycle_epoch;
        drop(state);
        self.lifecycle_sender.send_replace(lifecycle_epoch);
    }

    pub(crate) fn is_shut_down(&self) -> bool {
        self.state.lock().unwrap().runtime_lifecycle == RuntimeLifecycle::ShutDown
    }

    pub(crate) fn enqueue_prompt_control_draft(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        draft_id: crate::runtime::client::projection::CodingAgentDraftId,
        kind: crate::runtime::client::projection::CodingAgentControlKind,
    ) -> Result<
        crate::runtime::client::projection::CodingAgentControlReceipt,
        crate::runtime::client::projection::CodingAgentControlRejection,
    > {
        let text = {
            let mut state = self.state.lock().unwrap();
            let record = Self::record(&mut state, handle).map_err(|error| {
                crate::runtime::client::projection::CodingAgentControlRejection {
                    control_id: crate::runtime::client::projection::CodingAgentControlId(
                        draft_id.0.clone(),
                    ),
                    operation_id: operation_id.into(),
                    kind,
                    reason: control_rejection_reason(&error),
                }
            })?;
            let queue = match kind {
                crate::runtime::client::projection::CodingAgentControlKind::Steer => {
                    &record.steer_drafts
                }
                crate::runtime::client::projection::CodingAgentControlKind::FollowUp => {
                    &record.follow_up_drafts
                }
                crate::runtime::client::projection::CodingAgentControlKind::Abort => {
                    return Err(crate::runtime::client::projection::CodingAgentControlRejection {
                        control_id: crate::runtime::client::projection::CodingAgentControlId(draft_id.0),
                        operation_id: operation_id.into(), kind,
                        reason: crate::runtime::client::projection::CodingAgentControlRejectionReason::InvalidInput,
                    });
                }
            };
            queue
                .iter()
                .find(|draft| draft.id == draft_id.0)
                .map(|draft| draft.text.clone())
                .ok_or_else(|| crate::runtime::client::projection::CodingAgentControlRejection {
                    control_id: crate::runtime::client::projection::CodingAgentControlId(draft_id.0.clone()),
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::projection::CodingAgentControlRejectionReason::InvalidInput,
                })?
        };
        self.enqueue_control(
            handle,
            operation_id,
            crate::runtime::client::projection::CodingAgentControlId(draft_id.0),
            kind,
            text,
        )
    }

    pub(crate) fn enqueue_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::projection::CodingAgentControlId,
        kind: crate::runtime::client::projection::CodingAgentControlKind,
        text: String,
    ) -> Result<
        crate::runtime::client::projection::CodingAgentControlReceipt,
        crate::runtime::client::projection::CodingAgentControlRejection,
    > {
        self.enqueue_control_payload(
            handle,
            operation_id,
            control_id,
            kind,
            PromptControlPayload::Text(text),
        )
    }

    pub(crate) fn enqueue_content_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::projection::CodingAgentControlId,
        kind: crate::runtime::client::projection::CodingAgentControlKind,
        content: Vec<ai::api::conversation::ContentBlock>,
    ) -> Result<
        crate::runtime::client::projection::CodingAgentControlReceipt,
        crate::runtime::client::projection::CodingAgentControlRejection,
    > {
        self.enqueue_control_payload(
            handle,
            operation_id,
            control_id,
            kind,
            PromptControlPayload::Content(content),
        )
    }

    fn enqueue_control_payload(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::projection::CodingAgentControlId,
        kind: crate::runtime::client::projection::CodingAgentControlKind,
        payload: PromptControlPayload,
    ) -> Result<
        crate::runtime::client::projection::CodingAgentControlReceipt,
        crate::runtime::client::projection::CodingAgentControlRejection,
    > {
        if control_id.0.trim().is_empty() || payload.is_empty() {
            return Err(crate::runtime::client::projection::CodingAgentControlRejection {
                control_id,
                operation_id: operation_id.into(),
                kind,
                reason: crate::runtime::client::projection::CodingAgentControlRejectionReason::InvalidInput,
            });
        }
        let mut state = self.state.lock().unwrap();
        let record = match Self::record(&mut state, handle) {
            Ok(record) => record,
            Err(error @ ClientRegistryError::Lifecycle(_)) => {
                return Err(
                    crate::runtime::client::projection::CodingAgentControlRejection {
                        control_id,
                        operation_id: operation_id.into(),
                        kind,
                        reason: control_rejection_reason(&error),
                    },
                );
            }
            Err(_) => {
                return Err(crate::runtime::client::projection::CodingAgentControlRejection {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::projection::CodingAgentControlRejectionReason::InvalidInput,
                });
            }
        };
        let key = format!("{}:{}", operation_id, control_id.0);
        let signature = format!("{:?}:{}", kind, payload.signature());
        if let Some(stored) = record.control_receipts.get(&key) {
            if stored != &signature {
                return Err(crate::runtime::client::projection::CodingAgentControlRejection {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::projection::CodingAgentControlRejectionReason::PayloadConflict,
                });
            }
            return Ok(
                crate::runtime::client::projection::CodingAgentControlReceipt {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                },
            );
        }
        if record.control_receipts.len() >= MAX_RECEIPTS {
            return Err(crate::runtime::client::projection::CodingAgentControlRejection { control_id, operation_id: operation_id.into(), kind, reason: crate::runtime::client::projection::CodingAgentControlRejectionReason::QueueCapacityExceeded });
        }
        let queued_prepared_abort = if kind
            == crate::runtime::client::projection::CodingAgentControlKind::Abort
        {
            match record.prepared_operation.as_ref() {
                Some(prepared) if prepared.operation_id == operation_id => {
                    record.pending_abort_operation_id = Some(operation_id.to_owned());
                    true
                }
                Some(_) => {
                    return Err(
                            crate::runtime::client::projection::CodingAgentControlRejection {
                                control_id,
                                operation_id: operation_id.into(),
                                kind,
                                reason: crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetMismatch,
                            },
                        );
                }
                None => false,
            }
        } else {
            false
        };
        if !queued_prepared_abort {
            let dispatch = self.dispatch_control(handle, operation_id, kind, payload);
            if let Err(reason) = dispatch {
                let queued_running_abort = kind
                    == crate::runtime::client::projection::CodingAgentControlKind::Abort
                    && reason
                        == crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetNotRunning
                    && matches!(
                        record.submitted_operation.as_ref(),
                        Some(SubmittedOperationStatus::Running {
                            operation_id: running_operation_id,
                            ..
                        }) if running_operation_id == operation_id
                    );
                if queued_running_abort {
                    record.pending_abort_operation_id = Some(operation_id.to_owned());
                } else {
                    return Err(
                        crate::runtime::client::projection::CodingAgentControlRejection {
                            control_id,
                            operation_id: operation_id.into(),
                            kind,
                            reason,
                        },
                    );
                }
            }
        }
        record.control_receipts.insert(key.clone(), signature);
        record.control_receipt_order.push_back(key);
        let queue = match kind {
            crate::runtime::client::projection::CodingAgentControlKind::Steer => {
                Some(&mut record.steer_drafts)
            }
            crate::runtime::client::projection::CodingAgentControlKind::FollowUp => {
                Some(&mut record.follow_up_drafts)
            }
            crate::runtime::client::projection::CodingAgentControlKind::Abort => None,
        };
        if let Some(queue) = queue
            && let Some(position) = queue.iter().position(|draft| draft.id == control_id.0)
        {
            queue.remove(position);
        }
        Ok(
            crate::runtime::client::projection::CodingAgentControlReceipt {
                control_id,
                operation_id: operation_id.into(),
                kind,
            },
        )
    }

    fn dispatch_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        kind: crate::runtime::client::projection::CodingAgentControlKind,
        payload: PromptControlPayload,
    ) -> Result<(), crate::runtime::client::projection::CodingAgentControlRejectionReason> {
        let mut prompt_binding = self.prompt_control.lock().unwrap();
        if let Some(active) = prompt_binding.as_mut() {
            if active.owner.id != handle.id {
                return Err(
                    crate::runtime::client::projection::CodingAgentControlRejectionReason::NotOwner,
                );
            }
            if active.operation_id != operation_id {
                return Err(
                    crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetMismatch,
                );
            }
            return match (kind, payload) {
                (
                    crate::runtime::client::projection::CodingAgentControlKind::Abort,
                    PromptControlPayload::Text(reason),
                ) => active.sender.abort(reason),
                (
                    crate::runtime::client::projection::CodingAgentControlKind::Steer,
                    PromptControlPayload::Text(text),
                ) => active.sender.steer(text),
                (
                    crate::runtime::client::projection::CodingAgentControlKind::Steer,
                    PromptControlPayload::Content(content),
                ) => active.sender.steer_content(content),
                (
                    crate::runtime::client::projection::CodingAgentControlKind::FollowUp,
                    PromptControlPayload::Text(text),
                ) => active.sender.follow_up(text),
                (
                    crate::runtime::client::projection::CodingAgentControlKind::FollowUp,
                    PromptControlPayload::Content(content),
                ) => active.sender.follow_up_content(content),
                (
                    crate::runtime::client::projection::CodingAgentControlKind::Abort,
                    PromptControlPayload::Content(_),
                ) => Err(crate::runtime::facade::CodingSessionError::Input {
                    message: "abort control does not accept structured content".into(),
                }),
            }
            .map_err(|error| match error {
                crate::runtime::facade::CodingSessionError::Busy { .. } => {
                    crate::runtime::client::projection::CodingAgentControlRejectionReason::QueueCapacityExceeded
                }
                _ => crate::runtime::client::projection::CodingAgentControlRejectionReason::ControlChannelClosed,
            });
        }
        drop(prompt_binding);

        if kind != crate::runtime::client::projection::CodingAgentControlKind::Abort {
            return Err(
                crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetNotRunning,
            );
        }
        let cancellation_bindings = self.operation_cancellations.lock().unwrap();
        let Some(active) = cancellation_bindings.get(operation_id) else {
            if cancellation_bindings
                .values()
                .any(|active| active.owner.id == handle.id)
            {
                return Err(
                    crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetMismatch,
                );
            }
            return Err(
                crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetNotRunning,
            );
        };
        if active.owner.id != handle.id {
            return Err(
                crate::runtime::client::projection::CodingAgentControlRejectionReason::NotOwner,
            );
        }
        active.cancellation.request().map(|_| ()).map_err(|rejection| {
            match rejection {
                super::control::OperationIdentityRejection::CancellationClosed { .. } => {
                    crate::runtime::client::projection::CodingAgentControlRejectionReason::NoLongerCancellable
                }
                _ => crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetNotRunning,
            }
        })
    }

    pub(crate) fn bind_prompt_control(
        &self,
        owner: ClientHandle,
        operation_id: String,
        channel_generation: super::control::PromptControlGeneration,
        sender: PromptControlHandle,
    ) {
        *self.prompt_control.lock().unwrap() = Some(PromptControlBinding {
            owner,
            operation_id,
            channel_generation,
            sender,
        });
    }

    pub(crate) fn clear_prompt_control_if(
        &self,
        operation_id: &str,
        channel_generation: super::control::PromptControlGeneration,
    ) {
        let mut binding = self.prompt_control.lock().unwrap();
        if binding.as_ref().is_some_and(|active| {
            active.operation_id == operation_id && active.channel_generation == channel_generation
        }) {
            *binding = None;
        }
    }

    pub(crate) fn bind_operation_cancellation(
        &self,
        owner: ClientHandle,
        operation_id: String,
        cancellation: OperationCancellationHandle,
    ) {
        self.operation_cancellations.lock().unwrap().insert(
            operation_id.clone(),
            OperationCancellationBinding {
                owner: owner.clone(),
                cancellation,
            },
        );
        let pending_abort = {
            let mut state = self.state.lock().unwrap();
            state.clients.get_mut(&owner.id).and_then(|record| {
                (record.generation == owner.generation
                    && record.pending_abort_operation_id.as_deref() == Some(&operation_id))
                .then(|| record.pending_abort_operation_id.take())
                .flatten()
            })
        };
        if pending_abort.is_some()
            && let Some(active) = self
                .operation_cancellations
                .lock()
                .unwrap()
                .get(&operation_id)
        {
            let _ = active.cancellation.request();
        }
    }

    pub(crate) fn clear_operation_cancellation_if(&self, operation_id: &str) {
        self.operation_cancellations
            .lock()
            .unwrap()
            .remove(operation_id);
        let mut state = self.state.lock().unwrap();
        for record in state.clients.values_mut() {
            if record.pending_abort_operation_id.as_deref() == Some(operation_id) {
                record.pending_abort_operation_id = None;
            }
        }
    }

    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn connect_or_takeover(
        &self,
        id: ClientConnectionId,
    ) -> Result<ClientHandle, ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::validate_runtime(&state)?;
        state.shutdown_drain_eligibility.remove(&id);
        if let Some(record) = state.clients.get_mut(&id) {
            record.generation.0 += 1;
            record.connection = ConnectionLifecycle::Attached;
            let generation = record.generation;
            state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
            let lifecycle_epoch = state.lifecycle_epoch;
            let handle = ClientHandle { id, generation };
            drop(state);
            self.rebind_controls(&handle);
            self.lifecycle_sender.send_replace(lifecycle_epoch);
            return Ok(handle);
        }
        if state.clients.len() >= MAX_CLIENTS {
            return Err(ClientRegistryError::ClientCapacityExceeded { limit: MAX_CLIENTS });
        }
        let generation = ClientGeneration(1);
        state
            .clients
            .insert(id.clone(), ClientRecord::new(generation));
        Ok(ClientHandle { id, generation })
    }

    pub(crate) fn subscribe_lifecycle(&self) -> watch::Receiver<u64> {
        self.lifecycle_sender.subscribe()
    }

    pub(crate) fn validate_receiver(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let state = self.state.lock().unwrap();
        Self::validate_receiver_in_state(&state, handle, None)
    }

    pub(crate) fn validate_receiver_event(
        &self,
        handle: &ClientHandle,
        sequence: ProductEventSequence,
    ) -> Result<(), ClientRegistryError> {
        let state = self.state.lock().unwrap();
        Self::validate_receiver_in_state(&state, handle, Some(sequence))
    }

    fn validate_receiver_in_state(
        state: &SnapshotState,
        handle: &ClientHandle,
        sequence: Option<ProductEventSequence>,
    ) -> Result<(), ClientRegistryError> {
        if state.runtime_lifecycle == RuntimeLifecycle::ShutDown {
            if sequence.is_some_and(|sequence| {
                state
                    .shutdown_drain_boundary
                    .is_some_and(|boundary| sequence <= boundary)
            }) && state
                .shutdown_drain_eligibility
                .get(&handle.id)
                .is_some_and(|generation| *generation == handle.generation)
            {
                let record =
                    state
                        .clients
                        .get(&handle.id)
                        .ok_or(ClientRegistryError::Lifecycle(
                            CodingAgentLifecycleRejection::StaleGeneration,
                        ))?;
                if record.generation == handle.generation {
                    return Ok(());
                }
            }
            if state.clients.get(&handle.id).is_some_and(|record| {
                record.generation == handle.generation
                    && record.connection == ConnectionLifecycle::Detached
                    && state
                        .shutdown_drain_eligibility
                        .get(&handle.id)
                        .is_none_or(|generation| *generation != handle.generation)
            }) {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::RuntimeShutDown,
            ));
        }
        let record = state
            .clients
            .get(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached | ConnectionLifecycle::ShuttingDown => Ok(()),
            ConnectionLifecycle::Detached => Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::Detached,
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn shutdown_drain_boundary(&self) -> Option<ProductEventSequence> {
        self.state.lock().unwrap().shutdown_drain_boundary
    }

    fn rebind_controls(&self, handle: &ClientHandle) {
        let mut binding = self.prompt_control.lock().unwrap();
        if let Some(active) = binding.as_mut()
            && active.owner.id == handle.id
        {
            active.owner.generation = handle.generation;
        }
        drop(binding);
        let mut cancellations = self.operation_cancellations.lock().unwrap();
        for active in cancellations.values_mut() {
            if active.owner.id == handle.id {
                active.owner.generation = handle.generation;
            }
        }
    }

    pub(crate) fn detach(
        &self,
        handle: &ClientHandle,
    ) -> Result<ClientDetachOutcome, ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::validate_runtime(&state)?;
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return Ok(ClientDetachOutcome::StaleGeneration);
        };
        if record.generation != handle.generation {
            return Ok(ClientDetachOutcome::StaleGeneration);
        }
        match record.connection {
            ConnectionLifecycle::Attached => {
                record.connection = ConnectionLifecycle::Detached;
                state.shutdown_drain_eligibility.remove(&handle.id);
                state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
                let lifecycle_epoch = state.lifecycle_epoch;
                drop(state);
                self.lifecycle_sender.send_replace(lifecycle_epoch);
                Ok(ClientDetachOutcome::Detached)
            }
            ConnectionLifecycle::Detached => Ok(ClientDetachOutcome::AlreadyDetached),
            ConnectionLifecycle::ShuttingDown => {
                unreachable!("runtime validation rejects detach while the runtime is shutting down")
            }
        }
    }

    pub(crate) fn is_current(&self, handle: &ClientHandle) -> bool {
        let state = self.state.lock().unwrap();
        state
            .clients
            .get(&handle.id)
            .is_some_and(|record| record.generation == handle.generation)
    }

    pub(crate) fn install_projection(
        &self,
        session: CodingAgentSessionView,
        capabilities: CodingAgentCapabilities,
        capability_generation: CapabilityGeneration,
        committed_session_sequence: u64,
    ) {
        let _transition = self.capability_transition_guard();
        let mut state = self.state.lock().unwrap();
        let active_operation = state
            .projection
            .as_ref()
            .and_then(|projection| projection.active_operation);
        let revision = state
            .projection
            .as_ref()
            .map_or(1, |projection| projection.revision + 1);
        state.projection = Some(SnapshotProjection {
            revision,
            session,
            capabilities,
            active_operation,
            capability_generation,
        });
        state.capability_generation = capability_generation;
        state.committed_session_sequence = committed_session_sequence;
    }

    pub(crate) fn current_capability_generation(&self) -> CapabilityGeneration {
        self.state.lock().unwrap().capability_generation
    }

    pub(crate) fn capability_transition_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.capability_transition
            .lock()
            .expect("capability transition lock poisoned")
    }

    pub(crate) fn install_next_capability_generation(
        &self,
    ) -> Result<CapabilityGeneration, CodingSessionError> {
        let _transition = self.capability_transition_guard();
        let mut state = self.state.lock().unwrap();
        let next = state.capability_generation.next()?;
        state.capability_generation = next;
        if let Some(projection) = state.projection.as_mut() {
            projection.revision = projection.revision.checked_add(1).ok_or_else(|| {
                CodingSessionError::UnsupportedCapability {
                    capability: "snapshot projection revision is exhausted".into(),
                }
            })?;
            projection.capability_generation = next;
        }
        Ok(next)
    }

    pub(crate) fn set_active_operation(&self, active_operation: Option<OperationKind>) {
        let mut state = self.state.lock().unwrap();
        if let Some(projection) = state.projection.as_mut() {
            projection.revision += 1;
            projection.active_operation = active_operation;
        }
        if active_operation.is_none() {
            state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
            let lifecycle_epoch = state.lifecycle_epoch;
            drop(state);
            self.lifecycle_sender.send_replace(lifecycle_epoch);
        }
    }

    pub(crate) fn mark_recovery_projected(&self) {
        self.state.lock().unwrap().recovery_revision += 1;
    }

    pub(crate) fn snapshot(&self) -> UiSnapshot {
        self.snapshot_for_client(None)
            .expect("snapshot projection must be installed by session construction")
    }

    pub(crate) fn set_pending_authorizations(
        &self,
        pending: Vec<crate::authorization::ToolAuthorizationRequest>,
    ) {
        self.state.lock().unwrap().pending_authorizations = pending;
    }

    pub(crate) fn observe_context_event_in_state(
        state: &mut SnapshotState,
        event: &ProductEvent,
        operation_kind: Option<OperationKind>,
    ) {
        state
            .context_projection
            .apply_product_event(event, operation_kind);
    }

    pub(crate) fn client_snapshot(
        &self,
        handle: &ClientHandle,
    ) -> Result<UiSnapshot, ClientRegistryError> {
        self.snapshot_for_client(Some(handle))
    }

    pub(crate) fn client_state(
        &self,
        handle: &ClientHandle,
    ) -> Result<ClientSnapshotState, ClientRegistryError> {
        let snapshot = self.client_snapshot(handle)?;
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        let drafts = record
            .prompt_draft
            .iter()
            .chain(record.steer_drafts.iter())
            .chain(record.follow_up_drafts.iter())
            .cloned()
            .collect();
        Ok(ClientSnapshotState {
            snapshot,
            drafts,
            submitted_operation: record.submitted_operation.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn validate_prompt_draft(
        &self,
        handle: &ClientHandle,
        draft_id: &str,
        fingerprint: &str,
    ) -> Result<DraftRecord, ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        match &record.prompt_draft {
            Some(draft) if draft.id == draft_id && draft.fingerprint == fingerprint => {
                Ok(draft.clone())
            }
            _ => Err(ClientRegistryError::InvalidInput),
        }
    }

    pub(crate) fn validate_submission_slot(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        if record.prepared_operation.is_some() || record.submitted_operation.is_some() {
            Err(ClientRegistryError::SubmittedOperationPending)
        } else {
            Ok(())
        }
    }

    pub(crate) fn register_prepared_submission(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: OperationDescriptor,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        if record.prepared_operation.is_some() || record.submitted_operation.is_some() {
            return Err(ClientRegistryError::SubmittedOperationPending);
        }
        record.prepared_operation = Some(PreparedOperation {
            operation_id,
            descriptor,
        });
        Ok(())
    }

    pub(crate) fn abandon_prepared_submission(&self, handle: &ClientHandle, operation_id: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return;
        };
        if record.generation != handle.generation {
            return;
        }
        if record
            .prepared_operation
            .as_ref()
            .is_some_and(|prepared| prepared.operation_id == operation_id)
        {
            record.prepared_operation = None;
        }
        if record.pending_abort_operation_id.as_deref() == Some(operation_id) {
            record.pending_abort_operation_id = None;
        }
    }

    fn snapshot_for_client(
        &self,
        handle: Option<&ClientHandle>,
    ) -> Result<UiSnapshot, ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let client_drafts = match handle {
            Some(handle) => {
                let record = Self::record(&mut state, handle)?;
                record
                    .prompt_draft
                    .iter()
                    .chain(record.steer_drafts.iter())
                    .chain(record.follow_up_drafts.iter())
                    .map(|draft| ClientDraft::new(draft.kind, draft.text.clone()))
                    .collect()
            }
            None => Vec::new(),
        };
        let projection = state
            .projection
            .clone()
            .expect("snapshot projection must be installed by session construction");
        let recent_child_events = state
            .retained_product_events
            .iter()
            .filter(|event| {
                event.parent_operation_id().is_some()
                    || matches!(
                        event.event(),
                        crate::events::CodingAgentProductEventKind::Delegation(_)
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: state.event_stream_id.clone(),
                last_event_sequence: ProductEventSequence::new(
                    state.next_event_sequence.saturating_sub(1),
                ),
                last_session_sequence: state.committed_session_sequence,
                capability_generation: projection.capability_generation,
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            projection.session,
            projection.capabilities,
            projection.active_operation,
            client_drafts,
            state.pending_authorizations.clone(),
        )
        .with_context(state.context_projection.clone())
        .with_recent_child_events(recent_child_events))
    }

    fn record<'a>(
        state: &'a mut SnapshotState,
        handle: &ClientHandle,
    ) -> Result<&'a mut ClientRecord, ClientRegistryError> {
        Self::validate_runtime(state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached => {}
            ConnectionLifecycle::ShuttingDown => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::RuntimeShutDown,
                ));
            }
            ConnectionLifecycle::Detached => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
        }
        Ok(record)
    }

    fn validate_runtime(state: &SnapshotState) -> Result<(), ClientRegistryError> {
        match state.runtime_lifecycle {
            RuntimeLifecycle::Running => Ok(()),
            RuntimeLifecycle::ShuttingDown | RuntimeLifecycle::ShutDown => Err(
                ClientRegistryError::Lifecycle(CodingAgentLifecycleRejection::RuntimeShutDown),
            ),
        }
    }

    fn validate_terminal_runtime(state: &SnapshotState) -> Result<(), ClientRegistryError> {
        match state.runtime_lifecycle {
            RuntimeLifecycle::Running | RuntimeLifecycle::ShuttingDown => Ok(()),
            RuntimeLifecycle::ShutDown => Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::RuntimeShutDown,
            )),
        }
    }

    pub(crate) fn validate_client(
        state: &SnapshotState,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        Self::validate_runtime(state)?;
        let record = state
            .clients
            .get(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached => {}
            ConnectionLifecycle::ShuttingDown => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::RuntimeShutDown,
                ));
            }
            ConnectionLifecycle::Detached => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn live_lag_recovery(
        &self,
        handle: &ClientHandle,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        let state = self.state.lock().unwrap();
        match state.runtime_lifecycle {
            RuntimeLifecycle::ShutDown => {
                let boundary =
                    state
                        .shutdown_drain_boundary
                        .ok_or(ClientRegistryError::Lifecycle(
                            CodingAgentLifecycleRejection::RuntimeShutDown,
                        ))?;
                Self::shutdown_lag_recovery_from_state(&state, handle, boundary)
            }
            RuntimeLifecycle::Running | RuntimeLifecycle::ShuttingDown => {
                Self::validate_receiver_in_state(&state, handle, None)?;
                Self::lag_recovery_from_state(&state, handle)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn shutdown_lag_recovery(
        &self,
        handle: &ClientHandle,
        boundary: ProductEventSequence,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        let state = self.state.lock().unwrap();
        Self::shutdown_lag_recovery_from_state(&state, handle, boundary)
    }

    fn shutdown_lag_recovery_from_state(
        state: &SnapshotState,
        handle: &ClientHandle,
        boundary: ProductEventSequence,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        Self::validate_receiver_in_state(state, handle, Some(boundary))?;
        Self::lag_recovery_from_state(state, handle)
    }

    fn lag_recovery_from_state(
        state: &SnapshotState,
        handle: &ClientHandle,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        let record = state
            .clients
            .get(&handle.id)
            .filter(|record| record.generation == handle.generation)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        let projection = state
            .projection
            .clone()
            .expect("snapshot projection must be installed by session construction");
        let snapshot = UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: state.event_stream_id.clone(),
                last_event_sequence: ProductEventSequence::new(
                    state.next_event_sequence.saturating_sub(1),
                ),
                last_session_sequence: state.committed_session_sequence,
                capability_generation: projection.capability_generation,
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            projection.session,
            projection.capabilities,
            projection.active_operation,
            record
                .prompt_draft
                .iter()
                .chain(record.steer_drafts.iter())
                .chain(record.follow_up_drafts.iter())
                .map(|draft| ClientDraft::new(draft.kind, draft.text.clone()))
                .collect(),
            Vec::new(),
        )
        .with_context(state.context_projection.clone());
        let client_state = ClientSnapshotState {
            snapshot,
            drafts: record
                .prompt_draft
                .iter()
                .chain(record.steer_drafts.iter())
                .chain(record.follow_up_drafts.iter())
                .cloned()
                .collect(),
            submitted_operation: record.submitted_operation.clone(),
        };
        let oldest_available = state
            .retained_product_events
            .front()
            .map(ProductEvent::sequence)
            .unwrap_or_else(|| {
                client_state
                    .snapshot
                    .cursor
                    .last_event_sequence
                    .get()
                    .saturating_add(1)
            });
        Ok((client_state, oldest_available))
    }

    pub(crate) fn acknowledge(
        &self,
        handle: &ClientHandle,
        sequence: u64,
    ) -> Result<u64, ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        if sequence < record.acknowledged_sequence {
            return Ok(record.acknowledged_sequence);
        }
        record.acknowledged_sequence = sequence;
        if let Some(SubmittedOperationStatus::Terminal {
            anchor:
                SubmittedTerminalAnchor::ProductEvent {
                    sequence: terminal_sequence,
                    ..
                },
            ..
        }) = &record.submitted_operation
            && sequence >= *terminal_sequence
        {
            record.submitted_operation = None;
        }
        Ok(record.acknowledged_sequence)
    }

    pub(crate) fn acknowledge_outcome(
        &self,
        handle: &ClientHandle,
        acknowledgement: &crate::runtime::client::projection::CodingAgentOutcomeAcknowledgementId,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        match &record.submitted_operation {
            Some(SubmittedOperationStatus::Terminal {
                anchor:
                    SubmittedTerminalAnchor::OutcomeOnly {
                        acknowledgement: stored,
                    },
                ..
            }) if stored == acknowledgement => {
                record.submitted_operation = None;
                Ok(())
            }
            _ => Err(ClientRegistryError::InvalidInput),
        }
    }

    pub(crate) fn set_prompt_draft(
        &self,
        handle: &ClientHandle,
        draft: Option<DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::record(&mut state, handle)?.prompt_draft = draft;
        Ok(())
    }

    pub(crate) fn enqueue_draft(
        &self,
        handle: &ClientHandle,
        draft: DraftRecord,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        let queue = match draft.kind {
            crate::runtime::client::state::ClientDraftKind::Steer => &mut record.steer_drafts,
            crate::runtime::client::state::ClientDraftKind::FollowUp => {
                &mut record.follow_up_drafts
            }
            crate::runtime::client::state::ClientDraftKind::Prompt => {
                return Err(ClientRegistryError::InvalidInput);
            }
        };
        if queue.iter().any(|item| item.id == draft.id) {
            if let Some(item) = queue.iter_mut().find(|item| item.id == draft.id) {
                *item = draft;
            }
            return Ok(());
        }
        if queue.len() >= MAX_DRAFTS {
            return Err(ClientRegistryError::QueueCapacityExceeded { limit: MAX_DRAFTS });
        }
        queue.push_back(draft);
        Ok(())
    }

    pub(crate) fn clear_control_drafts(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        let record = Self::record(&mut state, handle)?;
        record.steer_drafts.clear();
        record.follow_up_drafts.clear();
        Ok(())
    }

    pub(crate) fn commit_submission_running(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: OperationDescriptor,
        expected_prompt_draft: Option<&DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        #[cfg(test)]
        if let Some(probe) = self.submission_transition_probe.lock().unwrap().take() {
            probe.entered.send(()).unwrap();
            probe.release.recv().unwrap();
        }
        let record = Self::record(&mut state, handle)?;
        if record.submitted_operation.is_some() {
            return Err(ClientRegistryError::SubmittedRegression);
        }
        match record.prepared_operation.as_ref() {
            Some(prepared)
                if prepared.operation_id == operation_id && prepared.descriptor == descriptor => {}
            _ => return Err(ClientRegistryError::SubmittedRegression),
        }
        match (descriptor.submitted_kind, expected_prompt_draft) {
            (OperationKind::Prompt, Some(expected))
                if record.prompt_draft.as_ref() == Some(expected) => {}
            (OperationKind::Prompt, _) => {
                return Err(ClientRegistryError::SubmissionDraftMismatch);
            }
            (_, None) => {}
            (_, Some(_)) => return Err(ClientRegistryError::InvalidInput),
        }
        record.submitted_operation = Some(SubmittedOperationStatus::Running {
            operation_id,
            kind: descriptor.submitted_kind,
            descriptor,
        });
        record.prepared_operation = None;
        if descriptor.submitted_kind == OperationKind::Prompt {
            record.prompt_draft = None;
        }
        Ok(())
    }

    pub(crate) fn abort_running_submission_if_matches(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return;
        };
        if !matches!(
            record.submitted_operation.as_ref(),
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                kind: stored_kind,
                descriptor: stored_descriptor,
            }) if stored_id == operation_id
                && *stored_kind == descriptor.submitted_kind
                && *stored_descriptor == descriptor
        ) {
            return;
        }
        record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
            operation_id: operation_id.to_owned(),
            kind: descriptor.submitted_kind,
            descriptor,
            anchor: SubmittedTerminalAnchor::TerminalUncertain {
                operation_id: operation_id.to_owned(),
            },
            status: ProductEventTerminalStatus::Aborted,
            root_count: 0,
        });
    }

    pub(crate) fn mark_recovery_pending(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
        recovery_id: String,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::SubmittedRegression)?;
        match record.submitted_operation.as_ref() {
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                record.submitted_operation = Some(SubmittedOperationStatus::RecoveryPending {
                    operation_id: operation_id.to_owned(),
                    kind: descriptor.submitted_kind,
                    descriptor,
                    recovery_id,
                });
                Ok(())
            }
            Some(SubmittedOperationStatus::RecoveryPending {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                recovery_id: stored_recovery_id,
                ..
            }) if stored_id == operation_id
                && *stored_descriptor == descriptor
                && stored_recovery_id == &recovery_id =>
            {
                Ok(())
            }
            _ => Err(ClientRegistryError::SubmittedRegression),
        }
    }

    #[cfg(test)]
    pub(crate) fn install_submission_transition_probe_for_tests(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self.submission_transition_probe.lock().unwrap() = Some(SubmissionTransitionProbe {
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }

    pub(crate) fn mark_terminal(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        kind: OperationKind,
        descriptor: OperationDescriptor,
        anchor: SubmittedTerminalAnchor,
        status: ProductEventTerminalStatus,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if !matches!(
            &record.submitted_operation,
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                kind: stored_kind,
                ..
            }) if stored_id == &operation_id && *stored_kind == kind
        ) {
            return Err(ClientRegistryError::SubmittedRegression);
        }
        record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
            operation_id: operation_id.clone(),
            kind,
            descriptor,
            anchor,
            status,
            root_count: 0,
        });
        Ok(())
    }

    pub(crate) fn observe_root_terminal_in_state(state: &mut SnapshotState, event: &ProductEvent) {
        let Some(operation_id) = event.operation_id() else {
            return;
        };
        let Some(terminal_operation) = event.terminal_operation() else {
            return;
        };
        let status = terminal_operation.status;
        for record in state.clients.values_mut() {
            let (stored_id, descriptor) = match record.submitted_operation.as_ref() {
                Some(SubmittedOperationStatus::Running {
                    operation_id,
                    descriptor,
                    ..
                })
                | Some(SubmittedOperationStatus::RecoveryPending {
                    operation_id,
                    descriptor,
                    ..
                })
                | Some(SubmittedOperationStatus::Terminal {
                    operation_id,
                    descriptor,
                    ..
                }) => (operation_id, *descriptor),
                None => continue,
            };
            if stored_id != operation_id {
                continue;
            }
            if crate::runtime::outcome::terminal_operation_kind(descriptor.submitted_kind)
                != Some(terminal_operation.kind)
            {
                continue;
            }
            match record.submitted_operation.as_mut() {
                Some(SubmittedOperationStatus::Terminal { root_count, .. }) => {
                    *root_count = root_count.saturating_add(1);
                }
                Some(
                    SubmittedOperationStatus::Running { .. }
                    | SubmittedOperationStatus::RecoveryPending { .. },
                ) => {
                    let durability = match event.durability() {
                        crate::events::CodingAgentProductEventDurability::PersistenceUncertain {
                            ..
                        } => SubmittedEventDurability::Uncertain,
                        _ => SubmittedEventDurability::Durable,
                    };
                    record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
                        operation_id: operation_id.to_owned(),
                        kind: descriptor.submitted_kind,
                        descriptor,
                        anchor: SubmittedTerminalAnchor::ProductEvent {
                            sequence: event.sequence(),
                            durability,
                        },
                        status,
                        root_count: 1,
                    });
                }
                None => {}
            }
        }
    }

    pub(crate) fn finalize_terminal_association(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
        fallback_status: ProductEventTerminalStatus,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.state.lock().unwrap();
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::SubmittedRegression)?;
        match record.submitted_operation.as_mut() {
            Some(SubmittedOperationStatus::Terminal {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                root_count,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                if *root_count == 1 {
                    Ok(())
                } else {
                    Err(ClientRegistryError::TerminalCardinality { count: *root_count })
                }
            }
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
                    operation_id: operation_id.to_owned(),
                    kind: descriptor.submitted_kind,
                    descriptor,
                    anchor: SubmittedTerminalAnchor::TerminalUncertain {
                        operation_id: operation_id.to_owned(),
                    },
                    status: fallback_status,
                    root_count: 0,
                });
                Ok(())
            }
            _ => Err(ClientRegistryError::SubmittedRegression),
        }
    }
}

fn control_rejection_reason(
    error: &ClientRegistryError,
) -> crate::runtime::client::projection::CodingAgentControlRejectionReason {
    use crate::runtime::client::projection::CodingAgentControlRejectionReason;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::outcome::OperationRootTerminalEvidence;

    fn draft(id: &str, kind: crate::runtime::client::state::ClientDraftKind) -> DraftRecord {
        DraftRecord {
            id: id.into(),
            kind,
            text: format!("{id}-text"),
            fingerprint: format!("{id}-text"),
        }
    }

    fn prompt_descriptor() -> OperationDescriptor {
        OperationDescriptor {
            revision: 1,
            submitted_kind: OperationKind::Prompt,
            dispatch_mode: super::super::operation::OperationDispatchMode::Async,
            outcome_family: crate::runtime::outcome::OperationOutcomeFamily::Prompt,
            terminal_policy: crate::runtime::outcome::OperationTerminalPolicy::ProductEvent,
            permitted_root_evidence: &[
                OperationRootTerminalEvidence::PromptCompleted,
                OperationRootTerminalEvidence::PromptFailed,
                OperationRootTerminalEvidence::PromptAborted,
            ],
            lineage: crate::runtime::outcome::OperationLineage::Root,
            session_access: crate::runtime::outcome::OperationSessionAccess::Write,
            runtime_access: crate::runtime::outcome::OperationRuntimeAccess::None,
            priority: crate::runtime::outcome::OperationPriority::Interactive,
            capacity: crate::runtime::outcome::OperationCapacity::SessionWriter,
            durability: crate::runtime::outcome::OperationDurability {
                session_if_persistent: true,
                runtime_generation: false,
            },
            cancellation: crate::runtime::outcome::OperationCancellation::Cancellable,
            child_policy: crate::runtime::outcome::OperationChildPolicy::Structured,
        }
    }

    #[test]
    fn snapshot_coordinator_owns_client_and_event_authority() {
        let coordinator = SnapshotCoordinator::new();
        let handle = coordinator
            .connect_or_takeover(ClientConnectionId::new("coordinator-client"))
            .unwrap();
        let state = coordinator.state.lock().unwrap();
        assert_eq!(state.clients.len(), 1);
        assert_eq!(state.clients[&handle.id].generation, handle.generation);
        assert_eq!(state.next_event_sequence, 1);
        assert!(state.retained_product_events.is_empty());
    }

    #[test]
    fn shutdown_drain_boundary_records_committed_shutdown_sequence() {
        let coordinator = SnapshotCoordinator::new();
        let events =
            crate::services::event::EventService::with_event_capacity_and_coordinator_for_tests(
                8,
                coordinator.clone(),
            );

        coordinator.request_shutdown();
        let shutdown = events.emit_runtime_shutdown();
        coordinator.finish_shutdown();

        assert_eq!(
            coordinator.shutdown_drain_boundary(),
            Some(shutdown.sequence_internal())
        );
    }

    #[tokio::test]
    async fn shutdown_lag_recovery_requires_phase_a_eligibility() {
        let mut session = crate::runtime::facade::CodingAgentSession::non_persistent(
            crate::runtime::facade::CodingAgentSessionOptions::new(),
        )
        .await
        .unwrap();
        let eligible = session
            .connect(
                crate::runtime::client::projection::CodingAgentClientId::new("eligible-lag-client"),
            )
            .unwrap();
        let ineligible = session
            .connect(
                crate::runtime::client::projection::CodingAgentClientId::new(
                    "ineligible-lag-client",
                ),
            )
            .unwrap();
        ineligible.detach().unwrap();

        session.runtime_shutdown_handle().request_shutdown();
        session.shutdown().await.unwrap();
        let boundary = session
            .runtime_host
            .client_projection
            .coordinator
            .shutdown_drain_boundary()
            .expect("shutdown event must establish a drain boundary");

        assert!(
            session
                .runtime_host
                .client_projection
                .coordinator
                .shutdown_lag_recovery(&eligible.handle(), boundary)
                .is_ok()
        );
        assert_eq!(
            session
                .runtime_host
                .client_projection
                .coordinator
                .shutdown_lag_recovery(&ineligible.handle(), boundary)
                .unwrap_err(),
            ClientRegistryError::Lifecycle(
                crate::runtime::error::CodingAgentLifecycleRejection::Detached
            )
        );
    }

    #[tokio::test]
    async fn shutdown_drain_eligibility_is_generation_scoped() {
        let mut session = crate::runtime::facade::CodingAgentSession::non_persistent(
            crate::runtime::facade::CodingAgentSessionOptions::new(),
        )
        .await
        .unwrap();
        let first = session
            .connect(
                crate::runtime::client::projection::CodingAgentClientId::new(
                    "generation-scoped-client",
                ),
            )
            .unwrap();
        let second = session
            .connect(
                crate::runtime::client::projection::CodingAgentClientId::new(
                    "generation-scoped-client",
                ),
            )
            .unwrap();

        session.runtime_shutdown_handle().request_shutdown();
        session.shutdown().await.unwrap();
        let boundary = session
            .runtime_host
            .client_projection
            .coordinator
            .shutdown_drain_boundary()
            .expect("shutdown event must establish a drain boundary");

        assert!(
            session
                .runtime_host
                .client_projection
                .coordinator
                .shutdown_lag_recovery(&second.handle(), boundary)
                .is_ok()
        );
        assert_eq!(
            session
                .runtime_host
                .client_projection
                .coordinator
                .shutdown_lag_recovery(&first.handle(), boundary)
                .unwrap_err(),
            ClientRegistryError::Lifecycle(
                crate::runtime::error::CodingAgentLifecycleRejection::RuntimeShutDown
            )
        );
    }

    #[test]
    fn snapshot_coordinator_capability_source_advances_monotonically() {
        let coordinator = SnapshotCoordinator::new();
        assert_eq!(
            coordinator.current_capability_generation(),
            CapabilityGeneration::new(1)
        );
        assert_eq!(
            coordinator.install_next_capability_generation().unwrap(),
            CapabilityGeneration::new(2)
        );
        assert_eq!(
            coordinator.current_capability_generation(),
            CapabilityGeneration::new(2)
        );
    }

    #[test]
    fn detach_is_idempotent_generation_scoped_and_preserves_reconnectable_facts() {
        let coordinator = SnapshotCoordinator::new();
        let id = ClientConnectionId::new("detach-client");
        let first = coordinator.connect_or_takeover(id.clone()).unwrap();
        coordinator.acknowledge(&first, 7).unwrap();
        coordinator
            .set_prompt_draft(
                &first,
                Some(draft(
                    "prompt",
                    crate::runtime::client::state::ClientDraftKind::Prompt,
                )),
            )
            .unwrap();
        coordinator
            .enqueue_draft(
                &first,
                draft(
                    "steer",
                    crate::runtime::client::state::ClientDraftKind::Steer,
                ),
            )
            .unwrap();
        let descriptor = prompt_descriptor();
        coordinator
            .register_prepared_submission(&first, "op-1".into(), descriptor)
            .unwrap();
        coordinator
            .commit_submission_running(
                &first,
                "op-1".into(),
                descriptor,
                Some(&draft(
                    "prompt",
                    crate::runtime::client::state::ClientDraftKind::Prompt,
                )),
            )
            .unwrap();

        assert_eq!(
            coordinator.detach(&first),
            Ok(ClientDetachOutcome::Detached)
        );
        assert_eq!(
            coordinator.detach(&first),
            Ok(ClientDetachOutcome::AlreadyDetached)
        );
        assert_eq!(
            coordinator.acknowledge(&first, 8),
            Err(ClientRegistryError::Lifecycle(
                crate::runtime::error::CodingAgentLifecycleRejection::Detached
            ))
        );

        let second = coordinator.connect_or_takeover(id).unwrap();
        assert_eq!(
            coordinator.detach(&first),
            Ok(ClientDetachOutcome::StaleGeneration)
        );
        let state = coordinator.state.lock().unwrap();
        let record = &state.clients[&second.id];
        assert_eq!(record.acknowledged_sequence, 7);
        assert_eq!(
            record.prompt_draft.iter().count()
                + record.steer_drafts.len()
                + record.follow_up_drafts.len(),
            1
        );
        assert!(matches!(
            record.submitted_operation,
            Some(SubmittedOperationStatus::Running { ref operation_id, .. })
                if operation_id == "op-1"
        ));
    }

    #[test]
    fn lifecycle_rejection_gate_rejects_state_draft_submission_replay_and_control() {
        let coordinator = SnapshotCoordinator::new();
        let handle = coordinator
            .connect_or_takeover(ClientConnectionId::new("lifecycle-client"))
            .unwrap();
        coordinator.detach(&handle).unwrap();
        let detached = ClientRegistryError::Lifecycle(
            crate::runtime::error::CodingAgentLifecycleRejection::Detached,
        );

        assert_eq!(coordinator.client_state(&handle).unwrap_err(), detached);
        assert_eq!(
            coordinator.set_prompt_draft(&handle, None),
            Err(detached.clone())
        );
        assert_eq!(
            coordinator.validate_prompt_draft(&handle, "missing", "missing"),
            Err(detached.clone())
        );
        assert_eq!(
            coordinator.commit_submission_running(&handle, "op".into(), prompt_descriptor(), None,),
            Err(detached.clone())
        );
        let state = coordinator.state.lock().unwrap();
        assert_eq!(
            SnapshotCoordinator::validate_client(&state, &handle),
            Err(detached)
        );
        drop(state);
        let rejection = coordinator
            .enqueue_control(
                &handle,
                "op",
                crate::runtime::client::projection::CodingAgentControlId("abort".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "stop".into(),
            )
            .unwrap_err();
        assert_eq!(
            rejection.reason,
            crate::runtime::client::projection::CodingAgentControlRejectionReason::Detached
        );
    }

    #[tokio::test]
    async fn detach_keeps_prompt_running_and_reconnect_rebinds_control() {
        let coordinator = SnapshotCoordinator::new();
        let id = ClientConnectionId::new("active-prompt-client");
        let first = coordinator.connect_or_takeover(id.clone()).unwrap();
        let active_draft = draft(
            "active-prompt",
            crate::runtime::client::state::ClientDraftKind::Prompt,
        );
        coordinator
            .set_prompt_draft(&first, Some(active_draft.clone()))
            .unwrap();
        let descriptor = prompt_descriptor();
        coordinator
            .register_prepared_submission(&first, "op-active".into(), descriptor)
            .unwrap();
        coordinator
            .commit_submission_running(&first, "op-active".into(), descriptor, Some(&active_draft))
            .unwrap();
        let (sender, mut receiver) = super::super::control::prompt_control_channel();
        coordinator.bind_prompt_control(
            first.clone(),
            "op-active".into(),
            super::super::control::PromptControlGeneration(1),
            sender,
        );

        assert_eq!(
            coordinator.detach(&first),
            Ok(ClientDetachOutcome::Detached)
        );
        let old_rejection = coordinator
            .enqueue_control(
                &first,
                "op-active",
                crate::runtime::client::projection::CodingAgentControlId("old-abort".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "old".into(),
            )
            .unwrap_err();
        assert_eq!(
            old_rejection.reason,
            crate::runtime::client::projection::CodingAgentControlRejectionReason::Detached
        );

        let second = coordinator.connect_or_takeover(id).unwrap();
        coordinator
            .enqueue_control(
                &second,
                "op-active",
                crate::runtime::client::projection::CodingAgentControlId("new-abort".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "new".into(),
            )
            .unwrap();
        assert_eq!(
            receiver.recv().await,
            Some(super::super::control::PromptControlCommand::Abort {
                reason: "new".into()
            })
        );

        coordinator.detach(&second).unwrap();
        coordinator
            .mark_terminal(
                &second,
                "op-active".into(),
                OperationKind::Prompt,
                prompt_descriptor(),
                SubmittedTerminalAnchor::ProductEvent {
                    sequence: 9,
                    durability: SubmittedEventDurability::Durable,
                },
                ProductEventTerminalStatus::Completed,
            )
            .unwrap();
        let third = coordinator
            .connect_or_takeover(ClientConnectionId::new("active-prompt-client"))
            .unwrap();
        let state = coordinator.state.lock().unwrap();
        assert!(matches!(
            state.clients[&third.id].submitted_operation,
            Some(SubmittedOperationStatus::Terminal { ref operation_id, .. })
                if operation_id == "op-active"
        ));
    }

    #[test]
    fn operation_cancellation_rebinds_to_the_latest_connection_generation() {
        let coordinator = SnapshotCoordinator::new();
        let id = ClientConnectionId::new("compact-control-client");
        let first = coordinator.connect_or_takeover(id.clone()).unwrap();
        let control =
            super::super::control::OperationControl::with_snapshot_coordinator(coordinator.clone());
        let operation = control
            .begin_root(
                crate::runtime::operation::OperationClass::SessionWriteRoot,
                OperationKind::Compact,
                "op-compact".into(),
            )
            .unwrap();
        let cancellation = operation.cancellation_token().unwrap();
        coordinator.bind_operation_cancellation(
            first.clone(),
            "op-compact".into(),
            operation.cancellation_handle(),
        );

        coordinator.detach(&first).unwrap();
        let second = coordinator.connect_or_takeover(id).unwrap();
        let stale = coordinator
            .enqueue_control(
                &first,
                "op-compact",
                crate::runtime::client::projection::CodingAgentControlId("abort-old".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "old generation".into(),
            )
            .unwrap_err();
        assert_eq!(
            stale.reason,
            crate::runtime::client::projection::CodingAgentControlRejectionReason::StaleGeneration
        );
        assert!(!cancellation.is_cancelled());

        coordinator
            .enqueue_control(
                &second,
                "op-compact",
                crate::runtime::client::projection::CodingAgentControlId("abort-new".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "new generation".into(),
            )
            .unwrap();
        assert!(cancellation.is_cancelled());

        coordinator.clear_operation_cancellation_if("op-compact");
        let completed = coordinator
            .enqueue_control(
                &second,
                "op-compact",
                crate::runtime::client::projection::CodingAgentControlId("abort-late".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "too late".into(),
            )
            .unwrap_err();
        assert_eq!(
            completed.reason,
            crate::runtime::client::projection::CodingAgentControlRejectionReason::TargetNotRunning
        );
    }

    #[test]
    fn operation_cancellation_routes_concurrent_roots_by_operation_id() {
        let coordinator = SnapshotCoordinator::new();
        let owner = coordinator
            .connect_or_takeover(ClientConnectionId::new("parallel-control-client"))
            .unwrap();
        let control =
            super::super::control::OperationControl::with_snapshot_coordinator(coordinator.clone());
        let first = control
            .begin_root(
                crate::runtime::operation::OperationClass::NonSessionRoot,
                OperationKind::AgentInvocation,
                "op-first".into(),
            )
            .unwrap();
        let second = control
            .begin_root(
                crate::runtime::operation::OperationClass::NonSessionRoot,
                OperationKind::AgentTeam,
                "op-second".into(),
            )
            .unwrap();
        coordinator.bind_operation_cancellation(
            owner.clone(),
            "op-first".into(),
            first.cancellation_handle(),
        );
        coordinator.bind_operation_cancellation(
            owner.clone(),
            "op-second".into(),
            second.cancellation_handle(),
        );

        coordinator
            .enqueue_control(
                &owner,
                "op-second",
                crate::runtime::client::projection::CodingAgentControlId("abort-second".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "stop second".into(),
            )
            .unwrap();

        assert!(!first.cancellation_token().unwrap().is_cancelled());
        assert!(second.cancellation_token().unwrap().is_cancelled());
    }

    #[test]
    fn operation_cancellation_reports_commit_gate_closure() {
        let coordinator = SnapshotCoordinator::new();
        let owner = coordinator
            .connect_or_takeover(ClientConnectionId::new("commit-gate-client"))
            .unwrap();
        let control =
            super::super::control::OperationControl::with_snapshot_coordinator(coordinator.clone());
        let operation = control
            .begin_root(
                crate::runtime::operation::OperationClass::RuntimeWrite,
                OperationKind::SetDefaultAgentProfile,
                "op-runtime-write".into(),
            )
            .unwrap();
        let cancellation = operation.cancellation_handle();
        cancellation.close().unwrap();
        coordinator.bind_operation_cancellation(
            owner.clone(),
            "op-runtime-write".into(),
            cancellation,
        );

        let rejection = coordinator
            .enqueue_control(
                &owner,
                "op-runtime-write",
                crate::runtime::client::projection::CodingAgentControlId("abort-late".into()),
                crate::runtime::client::projection::CodingAgentControlKind::Abort,
                "too late".into(),
            )
            .unwrap_err();
        assert_eq!(
            rejection.reason,
            crate::runtime::client::projection::CodingAgentControlRejectionReason::NoLongerCancellable
        );
    }

    #[test]
    fn prompt_control_cleanup_requires_operation_and_channel_generation() {
        let coordinator = SnapshotCoordinator::new();
        let owner_a = coordinator
            .connect_or_takeover(ClientConnectionId::new("prompt-cleanup-a"))
            .unwrap();
        let owner_b = coordinator
            .connect_or_takeover(ClientConnectionId::new("prompt-cleanup-b"))
            .unwrap();
        let generation_a = super::super::control::PromptControlGeneration(10);
        let generation_b = super::super::control::PromptControlGeneration(11);
        let (sender_a, _receiver_a) = super::super::control::prompt_control_channel();
        coordinator.bind_prompt_control(owner_a, "op-a".into(), generation_a, sender_a);

        coordinator.clear_prompt_control_if("op-other", generation_a);
        coordinator.clear_prompt_control_if("op-a", generation_b);
        assert!(coordinator.prompt_control.lock().unwrap().is_some());

        let (sender_b, mut receiver_b) = super::super::control::prompt_control_channel();
        coordinator.bind_prompt_control(owner_b, "op-b".into(), generation_b, sender_b);
        coordinator.clear_prompt_control_if("op-a", generation_a);
        let binding = coordinator.prompt_control.lock().unwrap().clone().unwrap();
        assert_eq!(binding.operation_id, "op-b");
        assert_eq!(binding.channel_generation, generation_b);
        binding.sender.follow_up("newer binding").unwrap();
        assert_eq!(
            receiver_b.try_recv().unwrap(),
            super::super::control::PromptControlCommand::FollowUp {
                text: "newer binding".into(),
            }
        );

        coordinator.clear_prompt_control_if("op-b", generation_b);
        coordinator.clear_prompt_control_if("op-b", generation_b);
        assert!(coordinator.prompt_control.lock().unwrap().is_none());
    }
}
