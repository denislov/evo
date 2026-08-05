mod connection;
pub(crate) mod context;
mod control;
mod lifecycle;
mod recovery;
mod view;

pub use crate::application::operation::contract::{
    BranchSummaryReusePolicy, CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOutcome,
};
pub use crate::application::operation::execution::CodingAgentOperationTask;
pub use crate::events::{
    CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentDiagnosticProductEvent, CodingAgentImageContent, CodingAgentMergeChange,
    CodingAgentMergeChangeKind, CodingAgentMergeProductEvent, CodingAgentMergeProposal,
    CodingAgentMessageProductEvent, CodingAgentProductEvent,
    CodingAgentProductEventCapabilityRevocation, CodingAgentProductEventCheckOutput,
    CodingAgentProductEventDeliveryClass, CodingAgentProductEventDiagnostic,
    CodingAgentProductEventDurability, CodingAgentProductEventError, CodingAgentProductEventFamily,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
    CodingAgentProductEventReplacement, CodingAgentProductEventTerminalOperation,
    CodingAgentProductEventTerminalOperationKind, CodingAgentProductEventTerminalStatus,
    CodingAgentProductEventUsage, CodingAgentRecoveryResolution, CodingAgentReviewChange,
    CodingAgentReviewHunk, CodingAgentReviewProductEvent, CodingAgentRuntimeProductEvent,
    CodingAgentSessionProductEvent, CodingAgentSessionWriteFailureReason,
    CodingAgentSessionWriteFailureStatus, CodingAgentTeamProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
#[allow(unused_imports)]
pub(crate) use crate::events::{ProductEvent, ProductEventSequence};
pub use crate::kernel::error::CodingAgentLifecycleRejection;
pub(crate) use crate::kernel::error::CodingSessionError;
pub use crate::operations::agent_invocation::runner::{
    AgentInvocationOptions, AgentInvocationOutcome,
};
pub use crate::operations::delegation::PendingDelegationConfirmation;
pub use crate::operations::export::{CodingAgentSessionExport, CodingAgentSessionExportItem};
pub use crate::operations::prompt::context::{PromptTurnMode, PromptTurnOptions};
pub use crate::operations::self_healing_edit::runner::{
    SelfHealingEditCheckOutput, SelfHealingEditDiagnostic, SelfHealingEditModelRepairOptions,
    SelfHealingEditOutcome, SelfHealingEditRepairAttempt, SelfHealingEditReplacement,
    SelfHealingEditRequest,
};
pub use crate::operations::team_invocation::runner::{
    AgentTeamMemberOutcome, AgentTeamOptions, AgentTeamOutcome,
};
pub use crate::profiles::{
    DelegationConfirmationMode, DelegationPolicy, ProfileId, ProfileKind, ProfileSource,
    SupervisionPolicy, TeamStrategy, TeamSupervisor,
};
pub(crate) use crate::profiles::{ProfileRegistry, ProfileRegistryOptions};
pub use crate::public_error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicDiagnostic,
    CodingAgentPublicDiagnosticOrigin, CodingAgentPublicDiagnosticSeverity, CodingAgentPublicError,
};
pub use crate::runtime::client::connection::{
    CodingAgentCapabilityControl, CodingAgentCapabilityRevocationOutcome,
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentConnectionGeneration,
    CodingAgentContextSnapshot, CodingAgentControlId, CodingAgentControlKind,
    CodingAgentControlReceipt, CodingAgentControlRejection, CodingAgentControlRejectionReason,
    CodingAgentDelegationSnapshot, CodingAgentDetachOutcome, CodingAgentDraft, CodingAgentDraftId,
    CodingAgentDraftKind, CodingAgentFileChangeSnapshot, CodingAgentFreshSnapshotRecovery,
    CodingAgentHunkChangeSnapshot, CodingAgentMutationRejection, CodingAgentOperationControl,
    CodingAgentOperationSnapshot, CodingAgentOperationStatus, CodingAgentOutcomeAcknowledgementId,
    CodingAgentPreparedSubmission, CodingAgentProductEventReceiver, CodingAgentPromptControl,
    CodingAgentReconnect, CodingAgentReconnectDelivery, CodingAgentReconnectReceiver,
    CodingAgentRecoveryReason, CodingAgentRuntimeShutdownHandle, CodingAgentShutdownOutcome,
    CodingAgentSnapshot, CodingAgentSnapshotCursor, CodingAgentSubmissionDraft,
    CodingAgentSubmittedEventDurability, CodingAgentSubmittedOperation,
    CodingAgentSubmittedOperationStatus, CodingAgentSubmittedTerminalAnchor,
    CodingAgentTerminalUncertainty, CodingAgentTurnUsageSnapshot, CodingAgentUsageSnapshot,
};
pub use crate::runtime::client::projection::{
    CodingAgentClientBootstrap, CodingAgentClientDiagnostic, CodingAgentClientMessage,
    CodingAgentClientMessageStatus, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    CodingAgentClientProjectionArea, CodingAgentClientProjectionChanges,
    CodingAgentClientProjectionIssue, CodingAgentClientProjectionLifecycle,
    CodingAgentClientRecovery, CodingAgentClientRecoveryStatus, CodingAgentClientTool,
    CodingAgentClientToolStatus, CodingAgentClientTranscript,
};
pub use crate::runtime::facade::context::{
    CapabilityStatus, CodingAgentCapabilities, CodingAgentRecoveryPending,
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryResolutionResult,
    CodingAgentRecoveryRetryRequest, CodingAgentRecoveryRetryResult, CodingAgentSessionNameUpdate,
    CodingAgentSessionNameUpdateReceiver, CodingAgentSessionOpenTarget, CodingAgentSessionOptions,
    CodingAgentSessionOverview, CodingAgentSessionSummary, CodingAgentSessionTranscriptItem,
    CodingAgentSessionView, CodingAgentTranscriptContinuation, CodingAgentTranscriptSnapshot,
    SessionStorageHandle,
};
pub(crate) use crate::runtime::facade::context::{
    CodingAgentSessionHydration, CodingAgentSessionTree,
};
pub use crate::runtime::facade::view::{
    CodingAgentAgentProfileSummary, CodingAgentTeamProfileSummary,
};
pub use crate::runtime::file_review::{
    CodingAgentExternalEditorTarget, CodingAgentFileChangeIdentity, CodingAgentFileReview,
    CodingAgentFileReviewActionRequest, CodingAgentFileReviewRequest, CodingAgentFileRevision,
    CodingAgentHunkReviewActionRequest,
};
pub use crate::runtime::version::{
    PRODUCT_EVENT_PROTOCOL_VERSION, ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION,
};
pub(crate) use crate::services::event::ProductEventReceiver;

use crate::application::snapshot as snapshot_coordinator;
use crate::runtime::client::connection as public_connection;

use crate::application::capability::CapabilitySnapshotService;
use crate::application::operation::control::{
    OperationControl, PromptControlCleanup, PromptControlGeneration,
};
pub(crate) use crate::application::operation::submission::SubmissionLeaseLifecycle;
use crate::application::session_coordinator::replay_derived_owner_state;
use crate::application::snapshot::SnapshotCoordinator;
pub(crate) use crate::operations::delegation::PendingDelegationConfirmationQueue;
use crate::operations::export::runner::ExportOptions;
use crate::runtime::client::service::ClientService;
use crate::runtime::intent::{IntentRouter, QueryIntent};
use crate::runtime::owners::RuntimeHost;
use crate::services::authorization::AuthorizationService;
use crate::services::event::EventService;
use crate::services::runtime::RuntimeService;
use crate::session::service::{
    SessionPersistence, SessionService, TransientSessionState, default_cwd, session_cwd,
};
pub(crate) use control::PromptControlCleanupGuard;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct CodingAgentSession {
    pub(crate) runtime_host: RuntimeHost,
}
