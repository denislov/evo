use crate::application::operation::control::OperationControl;
use crate::events::ProductEventSequence;
use crate::events::{CodingAgentProductEvent, CodingAgentProductEventTerminalStatus};
use crate::kernel::capability::{CapabilityRevocationPolicy, InstalledCapabilityGeneration};
use crate::kernel::error::CodingSessionError;
use crate::mutex::MutexExt;
use crate::public_error::CodingAgentPublicError;
use crate::runtime::client::state::{ClientConnectionId, ClientDraftKind};
use crate::runtime::facade::context::CodingAgentCapabilities;
use crate::runtime::version::{ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION};
use crate::services::event::{EventService, ProductEventReceiver, ProductEventRecovery};
use crate::session::view::CodingAgentSessionView;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::application::snapshot::{
    ClientDetachOutcome, ClientGeneration, ClientHandle, ClientRegistryError, ClientSnapshotState,
    DraftRecord, SnapshotCoordinator, SubmittedOperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodingAgentClientId(String);

impl CodingAgentClientId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodingAgentConnectionGeneration(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodingAgentDraftId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSubmissionDraft {
    id: CodingAgentDraftId,
    display_text: String,
}

impl CodingAgentSubmissionDraft {
    pub fn new(id: CodingAgentDraftId, display_text: impl Into<String>) -> Self {
        Self {
            id,
            display_text: display_text.into(),
        }
    }

    pub fn id(&self) -> &CodingAgentDraftId {
        &self.id
    }

    pub fn display_text(&self) -> &str {
        &self.display_text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentDraftKind {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentDraft {
    pub id: CodingAgentDraftId,
    pub kind: CodingAgentDraftKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodingAgentSubmittedOperationStatus {
    Running,
    RecoveryPending {
        recovery_id: String,
    },
    Terminal {
        status: CodingAgentProductEventTerminalStatus,
        anchor: CodingAgentSubmittedTerminalAnchor,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentSubmittedOperation {
    pub operation_id: String,
    pub kind: String,
    pub status: CodingAgentSubmittedOperationStatus,
}

/// Result of ending one connection generation without stopping runtime work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentDetachOutcome {
    Detached,
    AlreadyDetached,
    StaleGeneration,
}

/// Result of draining and closing the product runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentShutdownOutcome {
    ShutDown,
    AlreadyShutDown,
}

/// Cloneable Phase A authority that can stop new work while the unique owner is moved.
#[derive(Debug, Clone)]
pub struct CodingAgentRuntimeShutdownHandle {
    pub(crate) coordinator: Arc<SnapshotCoordinator>,
    pub(crate) authorization_service: crate::services::authorization::AuthorizationService,
}

impl CodingAgentRuntimeShutdownHandle {
    /// Idempotently close admission and resolve pending authorization waits without blocking.
    pub fn request_shutdown(&self) -> Result<(), CodingAgentPublicError> {
        self.coordinator
            .request_shutdown()
            .map_err(CodingAgentPublicError::from)?;
        self.authorization_service
            .cancel_all("tool authorization cancelled by runtime shutdown request")
            .map_err(CodingAgentPublicError::from)?;
        Ok(())
    }
}

/// Privileged runtime control for installing a new capability generation and
/// requesting cancellation of work admitted under older generations.
#[derive(Debug, Clone)]
pub struct CodingAgentCapabilityControl {
    pub(crate) coordinator: Arc<SnapshotCoordinator>,
    pub(crate) operation_control: OperationControl,
    pub(crate) event_service: EventService,
    pub(crate) authorization_service: crate::services::authorization::AuthorizationService,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentCapabilityRevocationOutcome {
    pub generation: u64,
    pub cancellation_requested_operation_ids: Vec<String>,
}

impl CodingAgentCapabilityControl {
    pub fn revoke_older_operations(
        &self,
    ) -> Result<CodingAgentCapabilityRevocationOutcome, CodingAgentPublicError> {
        self.revoke_older_operations_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn revoke_older_operations_internal(
        &self,
    ) -> Result<CodingAgentCapabilityRevocationOutcome, CodingSessionError> {
        let generation = self.coordinator.install_next_capability_generation()?;
        let cancellation_requested_operation_ids = self
            .operation_control
            .cancel_capability_generations_before(generation)?;
        for operation_id in &cancellation_requested_operation_ids {
            self.authorization_service.cancel_operation_blocking(
                operation_id,
                "tool authorization cancelled by capability revocation",
            )?;
        }
        self.event_service
            .emit_capability_changed(InstalledCapabilityGeneration {
                generation,
                revocation: CapabilityRevocationPolicy::RequestCancelOlderOperations,
                cancellation_requested_operation_ids: cancellation_requested_operation_ids.clone(),
            })?;
        Ok(CodingAgentCapabilityRevocationOutcome {
            generation: generation.get(),
            cancellation_requested_operation_ids,
        })
    }
}

/// Public durability evidence for a root terminal event.
///
/// This deliberately omits session identifiers and pending-write internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentSubmittedEventDurability {
    Durable,
    Uncertain,
}

/// Opaque identity used to acknowledge an outcome-only submission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodingAgentOutcomeAcknowledgementId(String);

impl CodingAgentOutcomeAcknowledgementId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Recovery disposition when no authoritative root terminal event was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentTerminalUncertainty {
    RecoveryRequired,
}

/// Exact public evidence that makes a submitted operation terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentSubmittedTerminalAnchor {
    ProductEvent {
        sequence: u64,
        durability: CodingAgentSubmittedEventDurability,
    },
    OutcomeOnly {
        acknowledgement: CodingAgentOutcomeAcknowledgementId,
    },
    TerminalUncertain {
        operation_id: String,
        recovery: CodingAgentTerminalUncertainty,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentRecoveryReason {
    RetainedHistoryGap,
    LiveReceiverLag,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentFreshSnapshotRecovery {
    pub requested_sequence: u64,
    pub oldest_available_sequence: u64,
    pub fresh_cursor: CodingAgentSnapshotCursor,
    pub reason: CodingAgentRecoveryReason,
    pub snapshot: Box<CodingAgentSnapshot>,
}

impl CodingAgentFreshSnapshotRecovery {
    pub fn into_public_error(self) -> CodingAgentPublicError {
        CodingAgentPublicError::from(CodingSessionError::EventStreamGap {
            requested_after: self.requested_sequence,
            oldest_available: self.oldest_available_sequence,
        })
    }
}

#[derive(Debug)]
pub enum CodingAgentReconnect {
    Replayed {
        events: Vec<CodingAgentProductEvent>,
        cursor: CodingAgentSnapshotCursor,
        receiver: CodingAgentReconnectReceiver,
    },
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodingAgentControlId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentControlKind {
    Abort,
    Steer,
    FollowUp,
    Interject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentControlReceipt {
    pub control_id: CodingAgentControlId,
    pub operation_id: String,
    pub kind: CodingAgentControlKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentControlRejectionReason {
    ResourceUnavailable,
    StaleConnection,
    Detached,
    StaleGeneration,
    RuntimeShutDown,
    NotOwner,
    TargetMismatch,
    TargetNotRunning,
    NoLongerCancellable,
    ControlChannelClosed,
    InvalidInput,
    QueueCapacityExceeded,
    PayloadConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentMutationRejection {
    QueueCapacity,
    ReceiptCapacity,
    TargetMismatch,
    TargetNotRunning,
    PayloadConflict,
    NotOwner,
    Detached,
    StaleGeneration,
    RuntimeShutDown,
    InvalidInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentControlRejection {
    pub control_id: CodingAgentControlId,
    pub operation_id: String,
    pub kind: CodingAgentControlKind,
    pub reason: CodingAgentControlRejectionReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingAgentOperationControl {
    pub client_id: CodingAgentClientId,
    pub generation: CodingAgentConnectionGeneration,
    pub operation_id: String,
    #[serde(skip, default = "SnapshotCoordinator::new")]
    pub(crate) coordinator: Arc<SnapshotCoordinator>,
}

impl PartialEq for CodingAgentOperationControl {
    fn eq(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.generation == other.generation
            && self.operation_id == other.operation_id
    }
}

impl Eq for CodingAgentOperationControl {}

impl CodingAgentOperationControl {
    pub fn abort(
        &self,
        control_id: CodingAgentControlId,
        reason: impl Into<String>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.coordinator.enqueue_control(
            &self.handle(),
            &self.operation_id,
            control_id,
            CodingAgentControlKind::Abort,
            reason.into(),
        )
    }

    fn handle(&self) -> ClientHandle {
        ClientHandle {
            id: internal_client_id(&self.client_id),
            generation: ClientGeneration(self.generation.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingAgentPromptControl {
    pub client_id: CodingAgentClientId,
    pub generation: CodingAgentConnectionGeneration,
    pub operation_id: String,
    #[serde(skip, default = "SnapshotCoordinator::new")]
    pub(crate) coordinator: Arc<SnapshotCoordinator>,
}

impl PartialEq for CodingAgentPromptControl {
    fn eq(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.generation == other.generation
            && self.operation_id == other.operation_id
    }
}

impl Eq for CodingAgentPromptControl {}

impl CodingAgentPromptControl {
    fn submit(
        &self,
        control_id: CodingAgentControlId,
        kind: CodingAgentControlKind,
        text: String,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.coordinator
            .enqueue_control(&self.handle(), &self.operation_id, control_id, kind, text)
    }

    fn submit_content(
        &self,
        control_id: CodingAgentControlId,
        kind: CodingAgentControlKind,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.coordinator.enqueue_content_control(
            &self.handle(),
            &self.operation_id,
            control_id,
            kind,
            content,
        )
    }

    pub fn abort(
        &self,
        control_id: CodingAgentControlId,
        reason: impl Into<String>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit(control_id, CodingAgentControlKind::Abort, reason.into())
    }

    pub fn steer(
        &self,
        control_id: CodingAgentControlId,
        text: impl Into<String>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit(control_id, CodingAgentControlKind::Steer, text.into())
    }

    pub fn steer_prepared(
        &self,
        control_id: CodingAgentControlId,
        prompt: crate::app::interactive::CodingAgentPreparedPrompt,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        match prompt.into_invocation() {
            crate::app::bootstrap::PromptInvocation::Text(text) => self.steer(control_id, text),
            crate::app::bootstrap::PromptInvocation::Content(content) => {
                self.submit_content(control_id, CodingAgentControlKind::Steer, content)
            }
            _ => unreachable!("prepared application prompt contains only text or content"),
        }
    }

    pub fn follow_up(
        &self,
        control_id: CodingAgentControlId,
        text: impl Into<String>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit(control_id, CodingAgentControlKind::FollowUp, text.into())
    }

    pub fn follow_up_prepared(
        &self,
        control_id: CodingAgentControlId,
        prompt: crate::app::interactive::CodingAgentPreparedPrompt,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        match prompt.into_invocation() {
            crate::app::bootstrap::PromptInvocation::Text(text) => self.follow_up(control_id, text),
            crate::app::bootstrap::PromptInvocation::Content(content) => {
                self.submit_content(control_id, CodingAgentControlKind::FollowUp, content)
            }
            _ => unreachable!("prepared application prompt contains only text or content"),
        }
    }

    pub fn steer_draft(
        &self,
        draft_id: CodingAgentDraftId,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit_draft(draft_id, CodingAgentControlKind::Steer)
    }

    pub fn follow_up_draft(
        &self,
        draft_id: CodingAgentDraftId,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit_draft(draft_id, CodingAgentControlKind::FollowUp)
    }

    pub fn interject(
        &self,
        control_id: CodingAgentControlId,
        text: impl Into<String>,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.submit(control_id, CodingAgentControlKind::Interject, text.into())
    }

    pub fn interject_prepared(
        &self,
        control_id: CodingAgentControlId,
        prompt: crate::app::interactive::CodingAgentPreparedPrompt,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        match prompt.into_invocation() {
            crate::app::bootstrap::PromptInvocation::Text(text) => self.interject(control_id, text),
            crate::app::bootstrap::PromptInvocation::Content(content) => {
                self.submit_content(control_id, CodingAgentControlKind::Interject, content)
            }
            _ => unreachable!("prepared application prompt contains only text or content"),
        }
    }

    fn submit_draft(
        &self,
        draft_id: CodingAgentDraftId,
        kind: CodingAgentControlKind,
    ) -> Result<CodingAgentControlReceipt, CodingAgentControlRejection> {
        self.coordinator.enqueue_prompt_control_draft(
            &self.handle(),
            &self.operation_id,
            draft_id,
            kind,
        )
    }

    fn handle(&self) -> ClientHandle {
        ClientHandle {
            id: internal_client_id(&self.client_id),
            generation: ClientGeneration(self.generation.0),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CodingAgentSubmissionLease {
    pub(crate) shared: Arc<Mutex<crate::runtime::facade::SubmissionLeaseLifecycle>>,
}

impl Drop for CodingAgentSubmissionLease {
    fn drop(&mut self) {
        self.abandon_if_prepared();
    }
}

impl CodingAgentSubmissionLease {
    fn abandon_if_prepared(&self) {
        // Drop cannot surface a resource error; recover only to release the
        // prepared submission and report the poisoned lock once.
        let mut lifecycle = self.shared.lock_or_recover("prepared submission lifecycle");
        if matches!(
            *lifecycle,
            crate::runtime::facade::SubmissionLeaseLifecycle::Prepared
        ) {
            *lifecycle = crate::runtime::facade::SubmissionLeaseLifecycle::Abandoned;
        }
    }
}

#[derive(Debug)]
pub struct CodingAgentPreparedSubmission {
    operation_id: String,
    operation: Option<crate::runtime::facade::CodingAgentOperation>,
    lease: CodingAgentSubmissionLease,
    owner: Arc<SnapshotCoordinator>,
    owner_handle: ClientHandle,
}

impl Drop for CodingAgentPreparedSubmission {
    fn drop(&mut self) {
        self.owner
            .abandon_prepared_submission(&self.owner_handle, &self.operation_id);
    }
}

impl CodingAgentPreparedSubmission {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub async fn run(
        self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<crate::runtime::facade::CodingAgentOperationOutcome, CodingAgentPublicError> {
        self.run_internal(session)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn run_internal(
        mut self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<crate::runtime::facade::CodingAgentOperationOutcome, CodingSessionError> {
        self.ensure_session_owner(session)?;
        let operation = self
            .operation
            .take()
            .expect("prepared submission owns exactly one operation");
        let outcome = session.run_internal(operation).await;
        self.cleanup_pre_admission_failure(session);
        outcome
    }

    pub fn submit(
        self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<crate::runtime::facade::CodingAgentOperationTask, CodingAgentPublicError> {
        self.submit_internal(session)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn submit_internal(
        mut self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<crate::runtime::facade::CodingAgentOperationTask, CodingSessionError> {
        self.ensure_session_owner(session)?;
        let operation = self
            .operation
            .take()
            .expect("prepared submission owns exactly one operation");
        let task = session.submit_internal(operation);
        self.cleanup_pre_admission_failure(session);
        task
    }

    pub fn discard(
        self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<(), CodingAgentPublicError> {
        self.discard_internal(session)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn discard_internal(
        self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) -> Result<(), CodingSessionError> {
        self.ensure_session_owner(session)?;
        self.cleanup_pre_admission_failure(session);
        Ok(())
    }

    fn cleanup_pre_admission_failure(
        &self,
        session: &mut crate::runtime::facade::CodingAgentSession,
    ) {
        self.lease.abandon_if_prepared();
        session.discard_submission_lease(&self.lease.shared);
        self.owner
            .abandon_prepared_submission(&self.owner_handle, &self.operation_id);
    }

    fn ensure_session_owner(
        &self,
        session: &crate::runtime::facade::CodingAgentSession,
    ) -> Result<(), CodingSessionError> {
        if session.owns_submission_coordinator(&self.owner) {
            Ok(())
        } else {
            Err(CodingSessionError::Input {
                message: "prepared submission belongs to a different session owner".into(),
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentSnapshotCursor {
    pub stream_id: String,
    pub snapshot_protocol_major: u32,
    pub last_event_sequence: u64,
    #[serde(default)]
    pub last_session_sequence: u64,
    pub capability_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentOperationStatus {
    Running,
    Completed,
    Failed,
    Aborted,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentOperationSnapshot {
    pub operation_id: String,
    pub kind: String,
    pub parent_operation_id: Option<String>,
    pub root_operation_id: Option<String>,
    pub status: CodingAgentOperationStatus,
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub diagnostics: Vec<String>,
    pub failure: Option<String>,
}

pub use crate::events::review::{
    CodingAgentReviewChange as CodingAgentFileChangeSnapshot,
    CodingAgentReviewHunk as CodingAgentHunkChangeSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentDelegationSnapshot {
    pub tool_call_id: String,
    pub child_operation_id: Option<String>,
    pub target_kind: String,
    pub target_id: String,
    pub task: String,
    pub status: String,
    pub updated_sequence: u64,
    pub summary: Option<String>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentTurnUsageSnapshot {
    pub turn_id: String,
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub context_tokens: Option<u32>,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentUsageSnapshot {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: Option<f64>,
    pub latest_turn: Option<CodingAgentTurnUsageSnapshot>,
    pub model_id: Option<String>,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentContextSnapshot {
    pub operations: Vec<CodingAgentOperationSnapshot>,
    pub changes: Vec<CodingAgentFileChangeSnapshot>,
    pub delegations: Vec<CodingAgentDelegationSnapshot>,
    pub usage: CodingAgentUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentSnapshot {
    pub cursor: CodingAgentSnapshotCursor,
    pub version: ProtocolFamilyVersion,
    pub session: CodingAgentSessionView,
    pub capabilities: CodingAgentCapabilities,
    pub active_operation: Option<String>,
    pub drafts: Vec<CodingAgentDraft>,
    pub submitted_operation: Option<CodingAgentSubmittedOperation>,
    pub pending_authorizations: Vec<crate::authorization::ToolAuthorizationRequest>,
    pub context: CodingAgentContextSnapshot,
}

#[derive(Debug, Clone)]
pub struct CodingAgentClientConnection {
    coordinator: Arc<SnapshotCoordinator>,
    event_service: EventService,
    authorization_service: crate::services::authorization::AuthorizationService,
    pub client_id: CodingAgentClientId,
    pub generation: CodingAgentConnectionGeneration,
    pub snapshot: CodingAgentSnapshot,
}

mod adapter;
mod receiver;
mod service;

pub(crate) use adapter::{internal_client_id, public_client_connection};
use adapter::{
    public_client_snapshot, registry_error, submission_preparation_error, validate_submission_draft,
};
pub use receiver::*;
