#![doc = include_str!("../README.md")]

mod app;
mod application;
mod authorization;
mod domain;
mod events;
mod kernel;
pub(crate) use kernel::limits;
mod mutex;
mod operations;
mod platform;
mod profiles;
mod public_error;
mod runtime;
mod services;

#[cfg(test)]
extern crate self as coding_agent;

mod config;
mod resources;
mod session;
mod theme;
mod tools;
mod workspace;

/// Stable, scenario-oriented library facade for embedding or scripting.
///
/// The categories below are the complete supported surface. Implementation
/// owners stay private, and this module intentionally has no flat re-exports.
pub mod api {
    /// Project configuration and runtime construction for in-process adapters.
    pub mod embedding {
        pub use crate::app::auth::{
            CodingAgentAuthCommand, CodingAgentAuthController, CodingAgentAuthMutation,
            CodingAgentAuthMutationOutcome, CodingAgentAuthSnapshot, CodingAgentProviderAuthKind,
            CodingAgentProviderAuthState, global_auth_snapshot,
        };
        pub use crate::app::embedding::{
            CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentEmbeddingSnapshot,
            CodingAgentModelCatalogEntry, CodingAgentModelChoice, CodingAgentProfileChoice,
            CodingAgentResourceCommand, CodingAgentResourceCommandKind, CodingAgentResourceSummary,
            CodingAgentSettingsSummary, CodingAgentThinkingCapability, CodingAgentThinkingLevel,
            CodingAgentThinkingLevelSanitization, configured_model_catalog,
            global_config_directory, global_skill_catalog, model_catalog,
            model_catalog_entry_by_id, sanitize_thinking_level,
        };
        pub use crate::app::interactive::{
            CodingAgentApplicationStartup, CodingAgentInteractiveStartup,
            CodingAgentPreparedPrompt, CodingAgentPromptImage,
        };
        pub use crate::app::invocation::{
            CodingAgentInvocationOptions, CodingAgentSessionSelection, CodingAgentToolExecutionMode,
        };
        pub use crate::app::profile_catalog::{
            CodingAgentAgentProfileCatalogEntry, CodingAgentProfileCatalog,
            CodingAgentProfileDelegationSummary, CodingAgentTeamProfileCatalogEntry,
        };
        pub use crate::app::session::CodingAgentSessionQuery;
        pub use crate::runtime::facade::CodingAgentSessionOpenTarget;
        pub use crate::workspace::{
            CodingAgentResolvedWorkspace, CodingAgentWorkspaceResolutionError,
            CodingAgentWorkspaceScope, CodingAgentWorkspaceSelection,
        };
        pub use workspace_runtime::api::{WorkspaceHandle, WorkspaceId, WorkspaceKind};
    }

    /// Bounded product runtime and adapter-presentation settings.
    pub mod settings {
        pub use crate::app::settings::{
            CodingAgentDoubleEscapeAction, CodingAgentPresentationSettingsSnapshot,
            CodingAgentQueueMode, CodingAgentRuntimeSettingsSnapshot, CodingAgentSettingsCommand,
            CodingAgentSettingsController, CodingAgentSettingsMutationOutcome,
            CodingAgentSettingsSnapshot, CodingAgentTreeFilterMode, global_settings_snapshot,
        };
        pub use crate::app::theme::{
            CodingAgentResolvedColor, CodingAgentThemeBackground, CodingAgentThemeController,
            CodingAgentThemeForeground, CodingAgentThemeReloadReceiver, CodingAgentThemeSnapshot,
            CodingAgentThemeWatcher,
        };
    }

    /// Tool invocation authorization requests and decisions.
    pub mod authorization {
        pub use crate::authorization::{
            ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationMode,
            ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
            ToolAuthorizationScope,
        };
    }

    /// Session lifecycle and the product runtime entry point.
    pub mod runtime {
        pub use crate::app::bootstrap::{SessionMode, SessionRunOptions};
        pub use crate::app::session::CodingAgentSessionBootstrap;
        pub use crate::runtime::facade::{
            CodingAgentCapabilityControl, CodingAgentCapabilityRevocationOutcome,
            CodingAgentOperationTask, CodingAgentRecoveryResolutionRequest,
            CodingAgentRecoveryResolutionResult, CodingAgentRecoveryRetryRequest,
            CodingAgentRecoveryRetryResult, CodingAgentRuntimeShutdownHandle, CodingAgentSession,
            CodingAgentSessionNameUpdate, CodingAgentSessionNameUpdateReceiver,
            CodingAgentSessionOptions, CodingAgentShutdownOutcome,
        };
    }

    /// Safe, bounded errors projected for product adapters.
    pub mod error {
        pub use crate::runtime::facade::{
            CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicDiagnostic,
            CodingAgentPublicDiagnosticOrigin, CodingAgentPublicDiagnosticSeverity,
            CodingAgentPublicError,
        };
    }

    /// Bounded changed-file review queries and validated presentation DTOs.
    pub mod review {
        pub use crate::runtime::facade::{
            CodingAgentExternalEditorTarget, CodingAgentFileChangeIdentity, CodingAgentFileReview,
            CodingAgentFileReviewRequest, CodingAgentFileRevision,
        };
    }

    /// Commands and outcomes accepted by [`runtime::CodingAgentSession`].
    pub mod operation {
        pub use crate::app::bootstrap::PromptInvocation;
        pub use crate::app::operation_factory::CodingAgentOperationFactory;
        pub use crate::app::prompt_execution::{
            CodingAgentPromptExecution, CodingAgentPromptExecutionMetadata,
            CodingAgentPromptExecutionPreparation, CodingAgentPromptExecutionStream,
            CodingAgentPromptExecutionUpdate,
        };
        pub use crate::runtime::facade::{
            AgentInvocationOptions, AgentInvocationOutcome, AgentTeamMemberOutcome,
            AgentTeamOptions, AgentTeamOutcome, BranchSummaryReusePolicy, CodingAgentOperation,
            CodingAgentOperationOutcome, DelegationConfirmationMode, DelegationPolicy,
            PendingDelegationConfirmation, PromptTurnMode, PromptTurnOptions, PromptTurnOutcome,
            SelfHealingEditCheckOutput, SelfHealingEditDiagnostic,
            SelfHealingEditModelRepairOptions, SelfHealingEditOutcome,
            SelfHealingEditRepairAttempt, SelfHealingEditReplacement, SelfHealingEditRequest,
            SupervisionPolicy,
        };
    }

    /// Durable and live product-event contracts.
    pub mod event {
        pub use crate::runtime::facade::{
            CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
            CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
            CodingAgentDiagnosticProductEvent, CodingAgentImageContent, CodingAgentMergeChange,
            CodingAgentMergeChangeKind, CodingAgentMergeProductEvent, CodingAgentMergeProposal,
            CodingAgentMessageProductEvent, CodingAgentProductEvent,
            CodingAgentProductEventCapabilityRevocation, CodingAgentProductEventCheckOutput,
            CodingAgentProductEventDeliveryClass, CodingAgentProductEventDiagnostic,
            CodingAgentProductEventDurability, CodingAgentProductEventError,
            CodingAgentProductEventFamily, CodingAgentProductEventKind,
            CodingAgentProductEventProfileKind, CodingAgentProductEventReceiver,
            CodingAgentProductEventReplacement, CodingAgentProductEventTerminalOperation,
            CodingAgentProductEventTerminalOperationKind, CodingAgentProductEventTerminalStatus,
            CodingAgentProductEventUsage, CodingAgentRecoveryResolution,
            CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent,
            CodingAgentSessionWriteFailureReason, CodingAgentSessionWriteFailureStatus,
            CodingAgentSubmittedEventDurability, CodingAgentTeamProductEvent,
            CodingAgentToolProductEvent, CodingAgentWorkflowProductEvent,
            PRODUCT_EVENT_PROTOCOL_VERSION,
        };
    }

    /// Client connection, submission, snapshot, and recovery contracts.
    pub mod client {
        pub use crate::runtime::facade::{
            CodingAgentClientBootstrap, CodingAgentClientConnection, CodingAgentClientDiagnostic,
            CodingAgentClientId, CodingAgentClientMessage, CodingAgentClientMessageStatus,
            CodingAgentClientProjection, CodingAgentClientProjectionApply,
            CodingAgentClientProjectionArea, CodingAgentClientProjectionChanges,
            CodingAgentClientProjectionIssue, CodingAgentClientProjectionLifecycle,
            CodingAgentClientRecovery, CodingAgentClientRecoveryStatus, CodingAgentClientTool,
            CodingAgentClientToolStatus, CodingAgentClientTranscript,
            CodingAgentConnectionGeneration, CodingAgentContextSnapshot, CodingAgentControlId,
            CodingAgentControlKind, CodingAgentControlReceipt, CodingAgentControlRejection,
            CodingAgentControlRejectionReason, CodingAgentDelegationSnapshot,
            CodingAgentDetachOutcome, CodingAgentDraft, CodingAgentDraftId, CodingAgentDraftKind,
            CodingAgentFileChangeSnapshot, CodingAgentFreshSnapshotRecovery,
            CodingAgentLifecycleRejection, CodingAgentMutationRejection,
            CodingAgentOperationControl, CodingAgentOperationSnapshot, CodingAgentOperationStatus,
            CodingAgentOutcomeAcknowledgementId, CodingAgentPreparedSubmission,
            CodingAgentPromptControl, CodingAgentReconnect, CodingAgentReconnectDelivery,
            CodingAgentReconnectReceiver, CodingAgentRecoveryPending, CodingAgentRecoveryReason,
            CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryResolutionResult,
            CodingAgentRecoveryRetryRequest, CodingAgentRecoveryRetryResult, CodingAgentSnapshot,
            CodingAgentSnapshotCursor, CodingAgentSubmissionDraft, CodingAgentSubmittedOperation,
            CodingAgentSubmittedOperationStatus, CodingAgentSubmittedTerminalAnchor,
            CodingAgentTerminalUncertainty, CodingAgentTurnUsageSnapshot, CodingAgentUsageSnapshot,
            ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION,
        };
    }

    /// Read-only product views and presentation DTOs.
    pub mod view {
        pub use crate::app::session::{
            CodingAgentSessionCatalog, CodingAgentSessionChoice, CodingAgentSessionChoiceKind,
            CodingAgentSessionOverviewCatalog, CodingAgentSessionSnapshot,
            CodingAgentSessionTreeNode, CodingAgentSessionTreeRole, CodingAgentSessionTreeSnapshot,
            CodingAgentSessionUsage,
        };
        pub use crate::runtime::facade::{
            CapabilityStatus, CodingAgentAgentProfileSummary, CodingAgentCapabilities,
            CodingAgentRecoveryPending, CodingAgentSessionExport, CodingAgentSessionExportItem,
            CodingAgentSessionOverview, CodingAgentSessionSummary,
            CodingAgentSessionTranscriptItem, CodingAgentSessionView,
            CodingAgentTeamProfileSummary, CodingAgentTranscriptContinuation,
            CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
            SessionStorageHandle, TeamStrategy, TeamSupervisor,
        };
        pub use crate::workspace::{
            CodingAgentWorkspaceKind, CodingAgentWorkspaceMigration,
            CodingAgentWorkspaceMigrationOutcome, CodingAgentWorkspaceOverview,
        };
    }
}

#[cfg(test)]
#[allow(deprecated)]
pub(crate) mod test_support;
