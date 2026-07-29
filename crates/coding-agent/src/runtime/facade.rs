mod connection;
pub(crate) mod context;
mod control;
mod lifecycle;
mod recovery;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod view;

pub use crate::events::{
    CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentDiagnosticProductEvent, CodingAgentImageContent, CodingAgentMessageProductEvent,
    CodingAgentProductEvent, CodingAgentProductEventCapabilityRevocation,
    CodingAgentProductEventCheckOutput, CodingAgentProductEventDeliveryClass,
    CodingAgentProductEventDiagnostic, CodingAgentProductEventDurability,
    CodingAgentProductEventError, CodingAgentProductEventFamily, CodingAgentProductEventKind,
    CodingAgentProductEventProfileKind, CodingAgentProductEventReplacement,
    CodingAgentProductEventTerminalOperation, CodingAgentProductEventTerminalOperationKind,
    CodingAgentProductEventTerminalStatus, CodingAgentProductEventUsage,
    CodingAgentProfileProductEvent, CodingAgentRecoveryResolution, CodingAgentRuntimeProductEvent,
    CodingAgentSessionProductEvent, CodingAgentSessionWriteFailureStatus,
    CodingAgentTeamProductEvent, CodingAgentToolProductEvent, CodingAgentWorkflowProductEvent,
};
#[allow(unused_imports)]
pub(crate) use crate::events::{ProductEvent, ProductEventSequence};
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
pub(crate) use crate::profiles::{AgentProfile, ProfileRegistry, ProfileRegistryOptions};
pub use crate::profiles::{
    DelegationConfirmationMode, DelegationPolicy, ProfileId, ProfileKind, ProfileSource,
    SupervisionPolicy, TeamStrategy, TeamSupervisor,
};
pub use crate::runtime::client::product_projection::{
    CodingAgentClientBootstrap, CodingAgentClientDiagnostic, CodingAgentClientMessage,
    CodingAgentClientMessageStatus, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    CodingAgentClientProjectionArea, CodingAgentClientProjectionChanges,
    CodingAgentClientProjectionIssue, CodingAgentClientProjectionLifecycle,
    CodingAgentClientRecovery, CodingAgentClientRecoveryStatus, CodingAgentClientTool,
    CodingAgentClientToolStatus, CodingAgentClientTranscript,
};
pub use crate::runtime::client::projection::{
    CodingAgentCapabilityControl, CodingAgentCapabilityRevocationOutcome,
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentConnectionGeneration,
    CodingAgentContextSnapshot, CodingAgentControlId, CodingAgentControlKind,
    CodingAgentControlReceipt, CodingAgentControlRejection, CodingAgentControlRejectionReason,
    CodingAgentDelegationSnapshot, CodingAgentDetachOutcome, CodingAgentDraft, CodingAgentDraftId,
    CodingAgentDraftKind, CodingAgentFileChangeSnapshot, CodingAgentFreshSnapshotRecovery,
    CodingAgentMutationRejection, CodingAgentOperationControl, CodingAgentOperationSnapshot,
    CodingAgentOperationStatus, CodingAgentOutcomeAcknowledgementId, CodingAgentPreparedSubmission,
    CodingAgentProductEventReceiver, CodingAgentPromptControl, CodingAgentReconnect,
    CodingAgentReconnectDelivery, CodingAgentReconnectReceiver, CodingAgentRecoveryReason,
    CodingAgentRuntimeShutdownHandle, CodingAgentShutdownOutcome, CodingAgentSnapshot,
    CodingAgentSnapshotCursor, CodingAgentSubmissionDraft, CodingAgentSubmittedEventDurability,
    CodingAgentSubmittedOperation, CodingAgentSubmittedOperationStatus,
    CodingAgentSubmittedTerminalAnchor, CodingAgentTerminalUncertainty,
    CodingAgentTurnUsageSnapshot, CodingAgentUsageSnapshot,
};
#[cfg(test)]
pub(crate) use crate::runtime::client::state::{
    ClientConnectionId, ClientDraft, ClientDraftKind, UiSnapshot,
};
pub use crate::runtime::error::CodingAgentLifecycleRejection;
pub(crate) use crate::runtime::error::CodingSessionError;
pub use crate::runtime::execution::CodingAgentOperationTask;
pub use crate::runtime::facade::context::{
    CapabilityStatus, CodingAgentCapabilities, CodingAgentRecoveryPending,
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryResolutionResult,
    CodingAgentRecoveryRetryRequest, CodingAgentRecoveryRetryResult, CodingAgentSessionOpenTarget,
    CodingAgentSessionOptions, CodingAgentSessionOverview, CodingAgentSessionSummary,
    CodingAgentSessionTranscriptItem, CodingAgentSessionView, CodingAgentTranscriptSnapshot,
};
pub(crate) use crate::runtime::facade::context::{
    CodingAgentSessionDiagnostic, CodingAgentSessionHydration, CodingAgentSessionTree,
    CodingAgentSessionUsageSummary,
};
pub use crate::runtime::facade::view::{
    CodingAgentAgentProfileSummary, CodingAgentTeamProfileSummary,
};
pub use crate::runtime::file_review::{
    CodingAgentExternalEditorTarget, CodingAgentFileChangeIdentity, CodingAgentFileReview,
    CodingAgentFileReviewRequest, CodingAgentFileRevision,
};
pub use crate::runtime::outcome::{
    BranchSummaryReusePolicy, CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOutcome,
};
pub use crate::runtime::public_error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicDiagnostic,
    CodingAgentPublicDiagnosticOrigin, CodingAgentPublicDiagnosticSeverity, CodingAgentPublicError,
};
pub use crate::runtime::version::{
    PRODUCT_EVENT_PROTOCOL_VERSION, ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION,
};
pub(crate) use crate::services::event::ProductEventReceiver;

use crate::runtime::client::projection as public_projection;
use crate::runtime::snapshot as snapshot_coordinator;

pub(crate) use crate::operations::delegation::{
    PendingDelegationConfirmationQueue, PendingDelegationConfirmationState,
    pending_state_from_replay,
};
use crate::operations::export::runner::ExportOptions;
use crate::runtime::capability::CapabilitySnapshotService;
pub use crate::runtime::capability::{FilesystemCapability, ShellCapability};
use crate::runtime::client::service::ClientService;
pub(crate) use crate::runtime::control::OperationKind;
#[cfg(test)]
pub(crate) use crate::runtime::control::PromptControlHandle;
use crate::runtime::control::{OperationControl, PromptControlCleanup, PromptControlGeneration};
use crate::runtime::intent::{IntentRouter, QueryIntent};
use crate::runtime::owners::RuntimeHost;
use crate::runtime::snapshot::SnapshotCoordinator;
pub(crate) use crate::runtime::submission::SubmissionLeaseLifecycle;
use crate::services::authorization::AuthorizationService;
use crate::services::capability::CapabilityService;
use crate::services::event::EventService;
use crate::services::runtime::RuntimeService;
use crate::services::session::{default_cwd, replay_derived_owner_state, session_cwd};
use crate::session::service::{SessionPersistence, SessionService, TransientSessionState};
pub(in crate::runtime) use control::PromptControlCleanupGuard;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct CodingAgentSession {
    pub(super) runtime_host: RuntimeHost,
}
