#![allow(
    dead_code,
    reason = "compile-pass fixture imports and constructs the stable facade without executing a test binary"
)]

use coding_agent::api::client::{
    CodingAgentClientBootstrap, CodingAgentClientConnection, CodingAgentClientProjection,
    CodingAgentClientProjectionApply, CodingAgentClientProjectionArea,
    CodingAgentClientProjectionIssue, CodingAgentClientTranscript,
    CodingAgentFreshSnapshotRecovery, CodingAgentSnapshot, CodingAgentSnapshotCursor,
    ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentAgentProfileCatalogEntry, CodingAgentApplicationStartup, CodingAgentAuthCommand,
    CodingAgentAuthController, CodingAgentAuthMutation, CodingAgentAuthMutationOutcome,
    CodingAgentAuthSnapshot, CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions,
    CodingAgentEmbeddingSnapshot, CodingAgentInteractiveStartup, CodingAgentInvocationOptions,
    CodingAgentModelCatalogEntry, CodingAgentModelChoice, CodingAgentPreparedPrompt,
    CodingAgentProfileCatalog, CodingAgentProfileDelegationSummary, CodingAgentPromptImage,
    CodingAgentProviderAuthKind, CodingAgentProviderAuthState, CodingAgentResourceCommand,
    CodingAgentResourceCommandKind, CodingAgentSessionQuery, CodingAgentSessionSelection,
    CodingAgentTeamProfileCatalogEntry, CodingAgentThinkingLevel, CodingAgentToolExecutionMode,
    configured_model_catalog, global_auth_snapshot, global_skill_catalog, model_catalog,
    model_catalog_entry_by_id,
};
use coding_agent::api::error::{CodingAgentPublicDiagnostic, CodingAgentPublicError};
use coding_agent::api::event::PRODUCT_EVENT_PROTOCOL_VERSION;
use coding_agent::api::operation::PromptInvocation;
use coding_agent::api::operation::{
    AgentInvocationOptions, AgentInvocationOutcome, AgentTeamMemberOutcome, AgentTeamOptions,
    AgentTeamOutcome, BranchSummaryReusePolicy, CodingAgentOperation, CodingAgentOperationFactory,
    CodingAgentOperationOutcome, CodingAgentPromptExecution, CodingAgentPromptExecutionPreparation,
    PendingDelegationConfirmation, PromptTurnOptions, PromptTurnOutcome, SelfHealingEditOutcome,
    SelfHealingEditReplacement, SelfHealingEditRequest,
};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileChangeIdentity, CodingAgentFileReview,
    CodingAgentFileReviewRequest, CodingAgentFileRevision,
};
use coding_agent::api::runtime::{
    CodingAgentOperationTask, CodingAgentSession, CodingAgentSessionBootstrap,
    CodingAgentSessionOptions,
};
use coding_agent::api::settings::{
    CodingAgentDoubleEscapeAction, CodingAgentPresentationMode,
    CodingAgentPresentationSettingsSnapshot, CodingAgentQueueMode, CodingAgentResolvedColor,
    CodingAgentRuntimeSettingsSnapshot, CodingAgentSettingsCommand, CodingAgentSettingsController,
    CodingAgentSettingsMutationOutcome, CodingAgentSettingsSnapshot, CodingAgentThemeBackground,
    CodingAgentThemeController, CodingAgentThemeForeground, CodingAgentThemeReloadReceiver,
    CodingAgentThemeSnapshot, CodingAgentThemeWatcher, CodingAgentTreeFilterMode,
    global_settings_snapshot,
};
use coding_agent::api::view::{
    CodingAgentAgentProfileSummary, CodingAgentSessionCatalog, CodingAgentSessionChoice,
    CodingAgentSessionChoiceKind, CodingAgentSessionExport, CodingAgentSessionOverview,
    CodingAgentSessionOverviewCatalog, CodingAgentSessionSnapshot, CodingAgentSessionSummary,
    CodingAgentSessionTreeNode, CodingAgentSessionTreeRole, CodingAgentSessionTreeSnapshot,
    CodingAgentSessionUsage, CodingAgentSessionView, CodingAgentTeamProfileSummary, ProfileId,
};

fn prompt() -> PromptTurnOptions {
    PromptTurnOptions::new(PromptInvocation::Text("fixture".into()))
}

fn session_queries(context: &CodingAgentEmbeddingContext) -> Result<(), CodingAgentPublicError> {
    let query: CodingAgentSessionQuery = context.session_query()?;
    touch(CodingAgentSessionQuery::global()?);
    touch(CodingAgentSessionQuery::from_session_root("sessions"));
    let bootstrap: CodingAgentSessionBootstrap = context.session_bootstrap();
    touch(bootstrap.clone().with_session_id("session"));
    touch(bootstrap.clone().with_new_session());
    touch(bootstrap.clone().with_fresh_session());
    touch(bootstrap.selected_snapshot()?);
    let catalog: CodingAgentSessionCatalog = query.catalog()?;
    let _: Option<CodingAgentSessionChoice> = catalog.choices.into_iter().next();
    let _: Option<CodingAgentSessionChoiceKind> = None;
    let overview_catalog: CodingAgentSessionOverviewCatalog = query.overviews()?;
    let _: Option<CodingAgentSessionOverview> = overview_catalog.overviews.into_iter().next();
    let _: Option<CodingAgentSessionSnapshot> = None;
    let _: Option<CodingAgentSessionTreeSnapshot> = None;
    let _: Option<CodingAgentSessionTreeNode> = None;
    let _: Option<CodingAgentSessionTreeRole> = None;
    let _: CodingAgentSessionUsage = Default::default();
    Ok(())
}

async fn create_assigned_session(
    context: &CodingAgentEmbeddingContext,
) -> Result<(), CodingAgentPublicError> {
    touch(CodingAgentSessionOptions::new().with_session_name("named session"));
    touch(context.create_session_with_id("assigned-session").await?);
    Ok(())
}

fn bind_operation_session(
    context: &CodingAgentEmbeddingContext,
    factory: &mut CodingAgentOperationFactory,
) {
    let bootstrap = context.session_bootstrap().with_new_session();
    factory.bind_session_bootstrap(&bootstrap);
}

fn auth_commands(
    context: &CodingAgentEmbeddingContext,
    factory: &mut CodingAgentOperationFactory,
) -> Result<(), CodingAgentPublicError> {
    let mut controller: CodingAgentAuthController = context.auth_controller();
    let _: CodingAgentAuthSnapshot = controller.snapshot();
    let outcome: CodingAgentAuthMutationOutcome =
        controller.apply(CodingAgentAuthCommand::remove_provider("provider"), factory)?;
    let _: CodingAgentAuthMutation = outcome.mutation;
    let _: Option<CodingAgentProviderAuthState> = outcome.snapshot.providers.into_iter().next();
    let _: Option<CodingAgentProviderAuthKind> = None;
    Ok(())
}

fn settings_commands(
    context: &CodingAgentEmbeddingContext,
    factory: &mut CodingAgentOperationFactory,
) -> Result<(), CodingAgentPublicError> {
    let mut controller: CodingAgentSettingsController = context.settings_controller();
    let _: CodingAgentSettingsSnapshot = controller.snapshot();
    let outcome: CodingAgentSettingsMutationOutcome = controller.apply(
        CodingAgentSettingsCommand::SetSteeringMode(CodingAgentQueueMode::All),
        factory,
    )?;
    let _: CodingAgentRuntimeSettingsSnapshot = outcome.snapshot.runtime;
    let _: CodingAgentPresentationSettingsSnapshot = outcome.snapshot.presentation;
    let _: Option<CodingAgentDoubleEscapeAction> = None;
    let _: Option<CodingAgentPresentationMode> = None;
    let _: Option<CodingAgentTreeFilterMode> = None;
    let _: CodingAgentSettingsCommand = CodingAgentSettingsCommand::SetSessionNamingModel(
        "claude-haiku-4-5".into(),
    );
    Ok(())
}

fn theme_projection() {
    let theme = CodingAgentThemeSnapshot::dark();
    let _: CodingAgentResolvedColor = theme.foreground(CodingAgentThemeForeground::Accent);
    let _: CodingAgentResolvedColor = theme.background(CodingAgentThemeBackground::UserMessage);
    let _: Option<CodingAgentThemeController> = None;
    let _: Option<CodingAgentThemeWatcher> = None;
    let _: Option<CodingAgentThemeReloadReceiver> = None;
    let _: Option<CodingAgentApplicationStartup> = None;
    let _: Option<CodingAgentInteractiveStartup> = None;
    let _: Option<CodingAgentPreparedPrompt> = None;
    let _: Option<CodingAgentPromptImage> = None;
    let _: Option<CodingAgentPromptExecution> = None;
    let _: Option<CodingAgentPromptExecutionPreparation> = None;
    let _: CodingAgentInvocationOptions = CodingAgentInvocationOptions::default();
    let _: CodingAgentSessionSelection = CodingAgentSessionSelection::Default;
    let _: CodingAgentToolExecutionMode = CodingAgentToolExecutionMode::Parallel;
}

fn profile_catalog(context: &CodingAgentEmbeddingContext) {
    let catalog: CodingAgentProfileCatalog = context.profile_catalog();
    let _: Option<&CodingAgentAgentProfileCatalogEntry> = catalog.agents.first();
    let _: Option<&CodingAgentTeamProfileCatalogEntry> = catalog.teams.first();
    let _: Option<&CodingAgentProfileDelegationSummary> =
        catalog.agents.first().map(|profile| &profile.delegation);
    touch(catalog.agent("default"));
    touch(catalog.team("team"));
}

fn product_projection(
    projection: &mut CodingAgentClientProjection,
    snapshot: CodingAgentSnapshot,
) -> Result<(), CodingAgentClientProjectionIssue> {
    let changes = projection.replace_snapshot(snapshot)?;
    touch(changes.contains(CodingAgentClientProjectionArea::Cursor));
    let _: Option<CodingAgentClientProjectionApply> = None;
    let _: Option<CodingAgentClientBootstrap> = None;
    let _: Option<CodingAgentClientTranscript> = None;
    Ok(())
}

fn operations() -> [CodingAgentOperation; 15] {
    [
        CodingAgentOperation::Prompt(prompt()),
        CodingAgentOperation::Compact(prompt()),
        CodingAgentOperation::BranchSummary {
            options: prompt(),
            source_leaf_id: "source".into(),
            target_leaf_id: "target".into(),
            custom_instructions: None,
            reuse: BranchSummaryReusePolicy::ReuseExisting,
        },
        CodingAgentOperation::SelfHealingEdit(SelfHealingEditRequest::new(
            "src/lib.rs",
            vec![SelfHealingEditReplacement::new("old", "new")],
        )),
        CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new("agent", "task", prompt())),
        CodingAgentOperation::InvokeTeam(AgentTeamOptions::new("team", "task", prompt())),
        CodingAgentOperation::SetDefaultAgentProfile {
            profile_id: ProfileId::from("agent"),
        },
        CodingAgentOperation::ApproveDelegation {
            operation_id: "operation".into(),
            tool_call_id: "tool".into(),
        },
        CodingAgentOperation::RejectDelegation {
            operation_id: "operation".into(),
            tool_call_id: "tool".into(),
            reason: "reason".into(),
        },
        CodingAgentOperation::ForkSession {
            target_leaf_id: None,
        },
        CodingAgentOperation::SwitchActiveLeaf {
            target_leaf_id: "leaf".into(),
        },
        CodingAgentOperation::SetSessionTreeLabel {
            entry_id: "leaf".into(),
            label: Some("checkpoint".into()),
        },
        CodingAgentOperation::SetSessionName {
            name: Some("planning".into()),
        },
        CodingAgentOperation::ExportCurrent,
        CodingAgentOperation::ExportCurrentHtml("session.html".into()),
    ]
}

fn outcomes(outcome: CodingAgentOperationOutcome) {
    match outcome {
        CodingAgentOperationOutcome::Prompt(value)
        | CodingAgentOperationOutcome::Compact(value) => touch(value),
        CodingAgentOperationOutcome::BranchSummary(value) => touch(value),
        CodingAgentOperationOutcome::SelfHealingEdit(value) => touch(value),
        CodingAgentOperationOutcome::AgentInvocation(value) => touch(value),
        CodingAgentOperationOutcome::AgentTeam(value) => touch(value),
        CodingAgentOperationOutcome::DefaultAgentProfileChanged
        | CodingAgentOperationOutcome::DelegationApproved
        | CodingAgentOperationOutcome::DelegationRejected
        | CodingAgentOperationOutcome::SessionForked
        | CodingAgentOperationOutcome::ActiveLeafSwitched => {}
        CodingAgentOperationOutcome::SessionTreeLabelChanged {
            entry_id,
            label,
            updated_at,
        } => touch((entry_id, label, updated_at)),
        CodingAgentOperationOutcome::SessionNameChanged { name, updated_at } => {
            touch((name, updated_at))
        }
        CodingAgentOperationOutcome::Export(value) => touch(value),
        CodingAgentOperationOutcome::ExportHtml(value) => touch(value),
    }
}

fn touch<T>(_: T) {}

fn support_types() {
    touch::<Option<PromptTurnOutcome>>(None);
    touch::<Option<SelfHealingEditOutcome>>(None);
    touch::<Option<AgentInvocationOutcome>>(None);
    touch::<Option<AgentTeamOutcome>>(None);
    touch::<Option<AgentTeamMemberOutcome>>(None);
    touch::<Option<CodingAgentSessionExport>>(None);
    touch::<Option<CodingAgentPublicError>>(None);
    touch::<Option<CodingAgentPublicDiagnostic>>(None);
    touch::<Option<CodingAgentAgentProfileSummary>>(None);
    touch::<Option<CodingAgentTeamProfileSummary>>(None);
    touch::<Option<CodingAgentSessionOptions>>(None);
    touch(CodingAgentSessionUsage::default().cost_known);
    touch::<Option<CodingAgentSessionSummary>>(None);
    touch::<Option<CodingAgentSessionView>>(None);
    touch::<Option<CodingAgentSnapshot>>(None);
    touch::<Option<CodingAgentSnapshotCursor>>(None);
    touch::<Option<CodingAgentFileChangeIdentity>>(None);
    touch::<Option<CodingAgentFileRevision>>(None);
    touch::<Option<CodingAgentFileReviewRequest>>(None);
    touch::<Option<CodingAgentFileReview>>(None);
    touch::<Option<CodingAgentExternalEditorTarget>>(None);
    touch::<Option<PendingDelegationConfirmation>>(None);
    touch::<Option<CodingAgentEmbeddingContext>>(None);
    touch::<Option<CodingAgentEmbeddingSnapshot>>(None);
    touch::<Option<CodingAgentProfileCatalog>>(None);
    touch::<Option<CodingAgentAgentProfileCatalogEntry>>(None);
    touch::<Option<CodingAgentTeamProfileCatalogEntry>>(None);
    touch::<Option<CodingAgentProfileDelegationSummary>>(None);
    touch::<Option<CodingAgentOperationFactory>>(None);
    touch::<Option<CodingAgentSessionBootstrap>>(None);
    touch::<Option<CodingAgentAuthController>>(None);
    touch::<Option<CodingAgentAuthCommand>>(None);
    touch::<Option<CodingAgentAuthMutationOutcome>>(None);
    touch::<Option<CodingAgentAuthSnapshot>>(None);
    touch::<Option<CodingAgentModelCatalogEntry>>(None);
    touch::<Option<CodingAgentResourceCommand>>(None);
    touch::<Option<CodingAgentResourceCommandKind>>(None);
    touch::<Option<CodingAgentModelChoice>>(None);
    touch(model_catalog());
    touch(model_catalog_entry_by_id("gpt-5"));
    touch(configured_model_catalog());
    touch(global_auth_snapshot());
    touch(global_skill_catalog());
    touch(global_settings_snapshot());
    touch(CodingAgentEmbeddingOptions::new("."));
    touch(CodingAgentThinkingLevel::High);
    touch(PRODUCT_EVENT_PROTOCOL_VERSION);
    touch(UI_SNAPSHOT_PROTOCOL_VERSION);
    touch::<Option<ProtocolFamilyVersion>>(None);
    let _: fn(&CodingAgentSession) -> Result<Option<std::path::PathBuf>, CodingAgentPublicError> =
        CodingAgentSession::session_storage_path;
    let _: fn(CodingAgentFreshSnapshotRecovery) -> CodingAgentPublicError =
        CodingAgentFreshSnapshotRecovery::into_public_error;
    let _: fn(&CodingAgentOperationTask, &CodingAgentClientConnection) =
        CodingAgentOperationTask::bind_control_owner;
    let _: fn(&CodingAgentClientConnection) -> Result<(), CodingAgentPublicError> =
        CodingAgentClientConnection::clear_control_drafts;
}

fn main() {
    assert_eq!(operations().len(), 14);
    support_types();
    let _ = profile_catalog;
    let _ = bind_operation_session;
    let _ = create_assigned_session;
}
