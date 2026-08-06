use ai_protocol::api::conversation::AssistantMessage;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::PromptInvocation;
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::admission::OperationScheduler;
use crate::application::operation::control::OperationControl;
use crate::kernel::capability::ActorId;
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::operations::delegation::worktree::{
    ChildWorkspaceBinding, ChildWorkspacePolicy, ChildWorktreeLease, bind_child_workspace,
};
use crate::operations::delegation::{
    DelegationAuthorizationDecision, DelegationLineageEntry, PendingDelegationConfirmationState,
    capability_snapshot_for_delegated_profile, delegation_lineage_for_request,
};
use crate::operations::prompt::context::{
    DelegationRequest, InternalPromptTurnOutcome, PromptTurnContext, PromptTurnIds,
    PromptTurnOptions,
};
use crate::operations::prompt::runner::PromptTurnRunner;
use crate::platform::time::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::profiles::{
    AgentProfile, ProfileId, ProfileKind, ProfileRegistry, TeamProfile, TeamSupervisor,
};
use crate::public_error::CodingAgentPublicDiagnostic;
use crate::services::authorization::AuthorizationService;
use crate::services::event::EventService;

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

#[derive(Debug, Clone)]
pub struct AgentTeamOptions {
    team_id: ProfileId,
    task: String,
    prompt_options: PromptTurnOptions,
    delegation_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
}

impl AgentTeamOptions {
    pub fn new(
        team_id: impl Into<ProfileId>,
        task: impl Into<String>,
        prompt_options: PromptTurnOptions,
    ) -> Self {
        Self {
            team_id: team_id.into(),
            task: task.into(),
            prompt_options,
            delegation_depth: 0,
            delegation_lineage: Vec::new(),
        }
    }

    pub fn with_delegation_depth(mut self, depth: usize) -> Self {
        self.delegation_depth = depth;
        self
    }

    pub(crate) fn with_delegation_lineage(mut self, lineage: Vec<DelegationLineageEntry>) -> Self {
        self.delegation_lineage = lineage;
        self
    }

    pub fn team_id(&self) -> &ProfileId {
        &self.team_id
    }

    pub fn task(&self) -> &str {
        &self.task
    }

    pub fn prompt_options(&self) -> &PromptTurnOptions {
        &self.prompt_options
    }

    pub(crate) fn prompt_options_mut(&mut self) -> &mut PromptTurnOptions {
        &mut self.prompt_options
    }

    pub fn delegation_depth(&self) -> usize {
        self.delegation_depth
    }

    pub(crate) fn delegation_lineage(&self) -> &[DelegationLineageEntry] {
        &self.delegation_lineage
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTeamMemberOutcome {
    pub profile_id: ProfileId,
    pub operation_id: String,
    pub turn_id: String,
    pub final_text: String,
    pub final_message: AssistantMessage,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTeamOutcome {
    pub operation_id: String,
    pub team_id: ProfileId,
    pub final_text: String,
    pub member_results: Vec<AgentTeamMemberOutcome>,
    pub supervisor_result: Option<Box<AgentTeamMemberOutcome>>,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

pub(crate) struct AgentTeamRunner;

impl AgentTeamRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut AgentTeamContext,
        cancellation: Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        let result: Result<(), CodingSessionError> = async {
            Self::check_cancellation(&cancellation)?;
            ctx.start_team()?;
            Self::check_cancellation(&cancellation)?;
            ctx.plan_subtasks()?;
            ctx.run_member_agents(cancellation.as_ref()).await?;
            Self::check_cancellation(&cancellation)?;
            ctx.collect_member_result()?;
            Self::check_cancellation(&cancellation)?;
            ctx.merge_or_reject_result(cancellation.as_ref()).await?;
            Self::check_cancellation(&cancellation)?;
            ctx.finalize_team()?;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            ctx.record_failure_terminal(error)?;
        }
        result
    }

    fn check_cancellation(
        cancellation: &Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        if cancellation
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(CodingSessionError::Cancelled);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct AgentTeamContext {
    options: AgentTeamOptions,
    registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    operation_id: String,
    team: Option<TeamProfile>,
    member_profiles: Vec<AgentProfile>,
    supervisor_profile: Option<AgentProfile>,
    member_results: Vec<AgentTeamMemberOutcome>,
    supervisor_result: Option<AgentTeamMemberOutcome>,
    final_text: Option<String>,
    parent_capability_snapshot: Option<OperationCapabilitySnapshot>,
    child_capability_snapshot: Option<OperationCapabilitySnapshot>,
    child_worktree: Option<ChildWorktreeLease>,
    pending_delegation_confirmations: Vec<PendingDelegationConfirmationState>,
    failure_terminal_recorded: bool,
    defer_terminal_publication: bool,
    authorization_service: Option<AuthorizationService>,
    extension_events: crate::services::ports::ExtensionEventDispatch,
}

impl AgentTeamContext {
    pub(crate) fn new(
        options: AgentTeamOptions,
        registry: ProfileRegistry,
        event_service: EventService,
        operation_control: OperationControl,
        operation_id: String,
    ) -> Self {
        Self {
            options,
            registry,
            event_service,
            operation_control,
            operation_id,
            team: None,
            member_profiles: Vec::new(),
            supervisor_profile: None,
            member_results: Vec::new(),
            supervisor_result: None,
            final_text: None,
            parent_capability_snapshot: None,
            child_capability_snapshot: None,
            child_worktree: None,
            pending_delegation_confirmations: Vec::new(),
            failure_terminal_recorded: false,
            defer_terminal_publication: false,
            authorization_service: None,
            extension_events: crate::services::ports::ExtensionEventDispatch::none(),
        }
    }

    pub(crate) fn with_extension_events(
        mut self,
        events: crate::services::ports::ExtensionEventDispatch,
    ) -> Self {
        self.extension_events = events;
        self
    }

    pub(crate) fn with_parent_capability_snapshot(
        mut self,
        snapshot: OperationCapabilitySnapshot,
    ) -> Self {
        self.parent_capability_snapshot = Some(snapshot);
        self
    }

    pub(crate) fn with_deferred_terminal_publication(mut self) -> Self {
        self.defer_terminal_publication = true;
        self
    }

    pub(crate) fn with_authorization_service(mut self, service: AuthorizationService) -> Self {
        self.authorization_service = Some(service);
        self
    }

    pub(crate) fn ensure_failure_terminal_draft(
        &self,
        error: &CodingSessionError,
    ) -> Result<(), CodingSessionError> {
        if self
            .event_service
            .has_deferred_terminal_draft(&self.operation_id)?
        {
            return Ok(());
        }
        let draft = if *error == CodingSessionError::Cancelled {
            EventService::agent_team_aborted_draft(
                self.operation_id.clone(),
                self.options.team_id.clone(),
                error.to_string(),
            )
        } else {
            EventService::agent_team_failed_draft(
                self.operation_id.clone(),
                self.options.team_id.clone(),
                error,
            )
        };
        self.event_service
            .defer_terminal_draft(self.operation_id.clone(), draft)?;
        Ok(())
    }

    pub(crate) fn take_pending_delegation_confirmations(
        &mut self,
    ) -> Vec<PendingDelegationConfirmationState> {
        std::mem::take(&mut self.pending_delegation_confirmations)
    }

    pub(crate) fn finish_success(&self) -> Result<AgentTeamOutcome, CodingSessionError> {
        Ok(AgentTeamOutcome {
            operation_id: self.operation_id.clone(),
            team_id: self.options.team_id.clone(),
            final_text: self
                .final_text
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "agent team completed without final text".into(),
                })?,
            member_results: self.member_results.clone(),
            supervisor_result: self.supervisor_result.clone().map(Box::new),
            diagnostics: self.all_diagnostics(),
        })
    }

    fn start_team(&mut self) -> Result<(), CodingSessionError> {
        if self.options.task.trim().is_empty() {
            return Err(CodingSessionError::Input {
                message: "agent team invocation requires a non-empty task".into(),
            });
        }
        self.event_service.emit_agent_team_started(
            self.operation_id.clone(),
            self.options.team_id.clone(),
            self.options.task.clone(),
        )?;
        Ok(())
    }

    fn plan_subtasks(&mut self) -> Result<(), CodingSessionError> {
        let team = self
            .registry
            .team(self.options.team_id.as_str())
            .cloned()
            .ok_or_else(|| CodingSessionError::Input {
                message: format!("Unknown team profile: {}", self.options.team_id),
            })?;
        if team.members.is_empty() {
            return Err(CodingSessionError::Input {
                message: format!("Team profile {} has no members", team.id),
            });
        }

        let mut member_profiles = Vec::new();
        for member_id in &team.members {
            let profile = self
                .registry
                .agent(member_id.as_str())
                .cloned()
                .ok_or_else(|| CodingSessionError::Input {
                    message: format!("Unknown team member agent profile: {member_id}"),
                })?;
            member_profiles.push(profile);
        }

        let supervisor_profile = match &team.supervisor {
            TeamSupervisor::Deterministic => None,
            TeamSupervisor::Agent(profile_id) => Some(
                self.registry
                    .agent(profile_id.as_str())
                    .cloned()
                    .ok_or_else(|| CodingSessionError::Input {
                        message: format!("Unknown team supervisor agent profile: {profile_id}"),
                    })?,
            ),
        };

        self.team = Some(team);
        self.member_profiles = member_profiles;
        self.supervisor_profile = supervisor_profile;
        Ok(())
    }

    async fn run_member_agents(
        &mut self,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        let members = self.member_profiles.clone();
        let task = self.options.task.clone();
        let concurrency = self.team_member_concurrency(members.len());
        let base = self.clone();
        let mut completed =
            futures::stream::iter(members.into_iter().enumerate().map(|(index, profile)| {
                let mut worker = base.clone();
                worker.member_results.clear();
                worker.pending_delegation_confirmations.clear();
                let task = task.clone();
                let cancellation = cancellation.cloned();
                async move {
                    let result = worker.run_profile_child(&profile, task, cancellation).await;
                    (
                        index,
                        result,
                        worker.pending_delegation_confirmations,
                        worker.child_capability_snapshot,
                    )
                }
            }))
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        completed.sort_by_key(|(index, _, _, _)| *index);
        for (_, result, pending, capability_snapshot) in completed {
            self.pending_delegation_confirmations.extend(pending);
            if capability_snapshot.is_some() {
                self.child_capability_snapshot = capability_snapshot;
            }
            self.member_results.push(result?);
        }
        Ok(())
    }

    /// Concurrency budget for parallel members.
    ///
    /// The bound comes from the managed-worktree registry capacity, never a
    /// hard-coded product constant: every write-capable member needs its own
    /// isolated worktree, so registry capacity is the natural concurrency
    /// budget. Without a configured registry the member count is used and
    /// provisioning fails closed per member.
    fn team_member_concurrency(&self, member_count: usize) -> usize {
        let capacity = self
            .operation_control
            .worktree_registry()
            .and_then(|registry| registry.capacity());
        capacity.map_or(member_count.max(1), |capacity| {
            capacity.min(member_count.max(1))
        })
    }

    fn collect_member_result(&mut self) -> Result<(), CodingSessionError> {
        if self.member_results.len() != self.member_profiles.len() {
            return Err(CodingSessionError::Session {
                message: "agent team member result collection is incomplete".into(),
            });
        }
        Ok(())
    }

    async fn merge_or_reject_result(
        &mut self,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        if let Some(supervisor) = self.supervisor_profile.clone() {
            let prompt = self.supervisor_prompt();
            let result = self
                .run_profile_child(&supervisor, prompt, cancellation.cloned())
                .await?;
            self.final_text = Some(result.final_text.clone());
            self.supervisor_result = Some(result);
        } else {
            self.final_text = Some(self.deterministic_final_text());
        }
        Ok(())
    }

    fn finalize_team(&mut self) -> Result<(), CodingSessionError> {
        let final_text = self
            .final_text
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "agent team cannot finalize without final text".into(),
            })?;
        for diagnostic in self.all_diagnostics() {
            self.event_service
                .emit_diagnostic(Some(self.operation_id.clone()), diagnostic.summary)?;
        }
        let draft = EventService::agent_team_completed_draft(
            self.operation_id.clone(),
            self.options.team_id.clone(),
            final_text,
        );
        if self.defer_terminal_publication {
            self.event_service
                .defer_terminal_draft(self.operation_id.clone(), draft)?;
        } else {
            self.event_service
                .emit_committed_terminal_draft(draft, OperationKind::AgentTeam)?;
        }
        Ok(())
    }

    async fn run_profile_child(
        &mut self,
        profile: &AgentProfile,
        prompt_text: String,
        cancellation: Option<CancellationToken>,
    ) -> Result<AgentTeamMemberOutcome, CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let child_operation_id = OperationScheduler::allocate_child_operation_id();
        let turn_id = ids.next_turn_id();
        let parent = self.parent_capability_snapshot.as_ref().ok_or_else(|| {
            CodingSessionError::UnsupportedCapability {
                capability: "team child operation requires an admitted parent capability snapshot"
                    .into(),
            }
        })?;
        let policy = ChildWorkspacePolicy::decide(parent, profile);
        let worktree_cancellation = cancellation.unwrap_or_default();
        let binding = match bind_child_workspace(
            &self.operation_control,
            parent,
            &child_operation_id,
            None,
            &worktree_cancellation,
            policy,
        )
        .await?
        {
            Some(lease) => {
                if worktree_cancellation.is_cancelled() {
                    let mut lease = lease;
                    lease.release()?;
                    return Err(CodingSessionError::Cancelled);
                }
                let handle = lease.handle()?;
                self.child_worktree = Some(lease);
                ChildWorkspaceBinding::Managed(handle)
            }
            None => match policy {
                ChildWorkspacePolicy::Projectless => ChildWorkspaceBinding::None,
                ChildWorkspacePolicy::ReadOnlyShared => ChildWorkspaceBinding::ReadOnlyShared,
                ChildWorkspacePolicy::Managed => {
                    unreachable!("managed policy always returns a lease or fails closed")
                }
            },
        };
        self.event_service.emit_agent_team_member_started(
            self.operation_id.clone(),
            child_operation_id.clone(),
            self.options.team_id.clone(),
            profile.id.clone(),
            prompt_text.clone(),
        )?;

        let mut prompt_options = self.options.prompt_options.clone();
        prompt_options.set_invocation(PromptInvocation::Text(prompt_text));
        if self.options.delegation_depth > 0 {
            prompt_options.apply_delegated_agent_profile(profile, &self.registry, Vec::new())?;
        } else {
            prompt_options.apply_agent_profile(profile, &self.registry, Vec::new())?;
        }
        if prompt_options.runtime().is_none() {
            return Err(CodingSessionError::Config {
                message: "agent team options do not include a runtime snapshot".into(),
            });
        }
        if let Some(handle) = binding.as_managed()
            && let Some(lease) = self.child_worktree.as_ref()
        {
            prompt_options.bind_child_workspace(lease.root().to_path_buf(), handle.clone())?;
        }

        let mut child_context = PromptTurnContext::new(
            PromptTurnIds::new(child_operation_id.clone(), turn_id),
            prompt_options,
        );
        child_context
            .set_non_persistent_session(format!("agent_team_{}", child_operation_id), Vec::new());
        child_context.enable_live_events(self.event_service.clone());
        if let Some(service) = self.authorization_service.clone() {
            child_context.set_authorization_service(service);
        }
        let parent = self.parent_capability_snapshot.as_ref().ok_or_else(|| {
            CodingSessionError::UnsupportedCapability {
                capability: "team child operation requires an admitted parent capability snapshot"
                    .into(),
            }
        })?;
        let capability_snapshot = capability_snapshot_for_delegated_profile(
            parent,
            child_operation_id.clone(),
            profile,
            ActorId::ChildOperation(parent.operation_id.clone()),
            binding,
        )?;
        let child_admission = OperationScheduler::admit_child(
            &self.operation_control,
            OperationKind::Prompt,
            capability_snapshot.clone(),
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(cancellation) = child_admission.cancellation_token() {
            child_context.set_operation_cancellation(cancellation);
        }
        child_context.set_capability_snapshot(capability_snapshot);
        if let Some(service) = self.authorization_service.clone() {
            crate::operations::delegation::execution::install_tool_executor(
                &mut child_context,
                self.registry.clone(),
                self.event_service.clone(),
                self.operation_control.clone(),
                service,
                self.options.delegation_depth,
                self.options.delegation_lineage.clone(),
            )?;
        }

        let mut finished_outcome = None;
        let child_delegations = match PromptTurnRunner::new()?.run_typed(&mut child_context).await {
            Ok(_) => {
                let decisions = if child_context.has_delegation_executor() {
                    Vec::new()
                } else {
                    child_context
                        .authorize_delegation_requests_with_lineage(
                            self.options.delegation_depth,
                            self.options.delegation_lineage(),
                        )?
                        .to_vec()
                };
                Some((
                    decisions,
                    child_context.options().clone(),
                    child_context.non_persistent_runtime_id().map(str::to_owned),
                ))
            }
            Err(error) => {
                finished_outcome = Some(match child_context.abort_reason() {
                    Some(reason) => child_context.finish_abort(
                        reason.to_owned(),
                        child_context.non_persistent_runtime_id().map(str::to_owned),
                    ),
                    None => child_context.finish_failure(error),
                });
                None
            }
        };
        let outcome = if let Some((decisions, prompt_options, runtime_id)) = child_delegations {
            self.child_capability_snapshot = child_context.capability_snapshot().cloned();
            if let Err(error) = self
                .execute_authorized_delegations(&decisions, prompt_options)
                .await
            {
                self.event_service.emit_diagnostic(
                    Some(child_operation_id.clone()),
                    format!("delegation execution failed: {error}"),
                )?;
            }
            child_context.finish_success(runtime_id, None)?
        } else {
            finished_outcome.ok_or_else(|| CodingSessionError::Session {
                message: "agent team child completed without prompt outcome".into(),
            })?
        };

        match outcome {
            InternalPromptTurnOutcome::Success {
                turn_id,
                final_text,
                final_message,
                diagnostics,
                ..
            } => {
                self.event_service
                    .emit_prompt_completed(child_operation_id.clone(), turn_id.clone())?;
                self.event_service.emit_agent_team_member_completed(
                    self.operation_id.clone(),
                    child_operation_id.clone(),
                    self.options.team_id.clone(),
                    profile.id.clone(),
                    final_text.clone(),
                )?;
                drop(child_admission);
                match self.promote_child_worktree(&child_operation_id) {
                    Ok(Some(proposal)) => {
                        self.event_service
                            .emit_merge_proposal_created(self.operation_id.clone(), proposal)?;
                        self.retain_child_worktree_for_merge();
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = self.release_child_worktree();
                        self.event_service
                            .emit_diagnostic(Some(child_operation_id.clone()), error.to_string())?;
                    }
                }
                Ok(AgentTeamMemberOutcome {
                    profile_id: profile.id.clone(),
                    operation_id: child_operation_id.clone(),
                    turn_id,
                    final_text,
                    final_message,
                    diagnostics: CodingAgentPublicDiagnostic::from_runtime_diagnostics(
                        &diagnostics,
                        Some(&child_operation_id),
                    ),
                })
            }
            InternalPromptTurnOutcome::Aborted { reason, .. } => {
                self.event_service
                    .emit_prompt_aborted(child_operation_id.clone(), reason.clone())?;
                drop(child_admission);
                self.release_child_worktree_diagnostic(&child_operation_id)?;
                Err(CodingSessionError::Cancelled)
            }
            InternalPromptTurnOutcome::Failed { error, .. } => {
                self.event_service
                    .emit_prompt_failed(child_operation_id.clone(), error.clone())?;
                drop(child_admission);
                self.release_child_worktree_diagnostic(&child_operation_id)?;
                Err(error)
            }
        }
    }

    fn release_child_worktree(&mut self) -> Result<(), CodingSessionError> {
        if let Some(lease) = self.child_worktree.as_mut() {
            lease.release()?;
        }
        Ok(())
    }

    /// Keep a successful member's worktree for review and merge.
    fn promote_child_worktree(
        &mut self,
        child_operation_id: &str,
    ) -> Result<Option<workspace_runtime::api::MergeProposal>, CodingSessionError> {
        if let Some(lease) = self.child_worktree.as_mut() {
            Ok(Some(lease.promote_to_merge_pending(child_operation_id)?))
        } else {
            Ok(None)
        }
    }

    fn retain_child_worktree_for_merge(&mut self) {
        if let Some(lease) = self.child_worktree.as_mut() {
            lease.retain_for_merge();
        }
    }

    fn release_child_worktree_diagnostic(
        &mut self,
        child_operation_id: &str,
    ) -> Result<(), CodingSessionError> {
        if let Some(lease) = self.child_worktree.as_mut()
            && let Err(error) = lease.release()
        {
            self.event_service.emit_diagnostic(
                Some(child_operation_id.to_owned()),
                format!("child worktree release failed: {error}"),
            )?;
        }
        Ok(())
    }

    async fn execute_authorized_delegations(
        &mut self,
        decisions: &[DelegationAuthorizationDecision],
        prompt_options: PromptTurnOptions,
    ) -> Result<(), CodingSessionError> {
        for decision in decisions {
            match decision {
                DelegationAuthorizationDecision::Approved {
                    request,
                    child_delegation_depth,
                } => {
                    self.event_service.emit_delegation_approved(request)?;
                    match request.target_kind {
                        ProfileKind::Agent => {
                            self.execute_approved_agent_delegation(
                                request,
                                prompt_options.clone(),
                                *child_delegation_depth,
                            )
                            .await?;
                        }
                        ProfileKind::Team => {
                            self.execute_approved_team_delegation(
                                request,
                                prompt_options.clone(),
                                *child_delegation_depth,
                            )
                            .await?;
                        }
                    }
                }
                DelegationAuthorizationDecision::RequiresConfirmation {
                    request,
                    reason,
                    child_delegation_depth,
                } => {
                    self.pending_delegation_confirmations.push(
                        PendingDelegationConfirmationState {
                            request: request.clone(),
                            prompt_options: prompt_options.clone(),
                            reason: reason.clone(),
                            requested_at: SystemClock.now_rfc3339(),
                            child_delegation_depth: *child_delegation_depth,
                            delegation_lineage: delegation_lineage_for_request(
                                self.options.delegation_lineage(),
                                request,
                            ),
                        },
                    );
                    self.event_service
                        .emit_delegation_confirmation_required(request, reason)?;
                }
                DelegationAuthorizationDecision::Rejected { request, reason } => {
                    self.event_service
                        .emit_delegation_rejected(request, reason)?;
                }
            }
        }
        Ok(())
    }

    async fn execute_approved_agent_delegation(
        &mut self,
        request: &DelegationRequest,
        prompt_options: PromptTurnOptions,
        child_delegation_depth: usize,
    ) -> Result<(), CodingSessionError> {
        let outcome = Box::pin(crate::operations::delegation::execution::execute_agent(
            self.registry.clone(),
            self.event_service.clone(),
            self.operation_control.clone(),
            request,
            prompt_options,
            child_delegation_depth,
            delegation_lineage_for_request(self.options.delegation_lineage(), request),
            self.child_capability_snapshot.clone(),
            self.authorization_service.clone(),
            None,
            self.extension_events.clone(),
        ))
        .await;
        self.pending_delegation_confirmations
            .extend(outcome.pending_confirmations);
        outcome.execution.map(|_| ())
    }

    async fn execute_approved_team_delegation(
        &mut self,
        request: &DelegationRequest,
        prompt_options: PromptTurnOptions,
        child_delegation_depth: usize,
    ) -> Result<(), CodingSessionError> {
        let outcome = Box::pin(crate::operations::delegation::execution::execute_team(
            self.registry.clone(),
            self.event_service.clone(),
            self.operation_control.clone(),
            request,
            prompt_options,
            child_delegation_depth,
            delegation_lineage_for_request(self.options.delegation_lineage(), request),
            self.child_capability_snapshot.clone(),
            self.authorization_service.clone(),
            None,
            self.extension_events.clone(),
        ))
        .await;
        self.pending_delegation_confirmations
            .extend(outcome.pending_confirmations);
        outcome.execution.map(|_| ())
    }

    fn deterministic_final_text(&self) -> String {
        let mut lines = vec![format!("Team {} completed.", self.options.team_id)];
        for result in &self.member_results {
            lines.push(String::new());
            lines.push(format!("[{}]", result.profile_id));
            lines.push(result.final_text.clone());
        }
        lines.join("\n")
    }

    fn supervisor_prompt(&self) -> String {
        let mut lines = vec![
            "You are supervising an agent team.".to_string(),
            String::new(),
            format!("Task: {}", self.options.task),
            String::new(),
            "Member results:".to_string(),
        ];
        for result in &self.member_results {
            lines.push(format!("- {}: {}", result.profile_id, result.final_text));
        }
        lines.push(String::new());
        lines.push("Produce the final team response.".to_string());
        lines.join("\n")
    }

    fn all_diagnostics(&self) -> Vec<CodingAgentPublicDiagnostic> {
        let mut diagnostics = Vec::new();
        for result in &self.member_results {
            diagnostics.extend(result.diagnostics.clone());
        }
        if let Some(result) = &self.supervisor_result {
            diagnostics.extend(result.diagnostics.clone());
        }
        diagnostics
    }

    fn record_failure_terminal(
        &mut self,
        error: &CodingSessionError,
    ) -> Result<(), CodingSessionError> {
        if !self.failure_terminal_recorded {
            self.failure_terminal_recorded = true;
            match error {
                CodingSessionError::Cancelled => {
                    let draft = EventService::agent_team_aborted_draft(
                        self.operation_id.clone(),
                        self.options.team_id.clone(),
                        error.to_string(),
                    );
                    if self.defer_terminal_publication {
                        self.event_service
                            .defer_terminal_draft(self.operation_id.clone(), draft)?;
                    } else {
                        self.event_service
                            .emit_committed_terminal_draft(draft, OperationKind::AgentTeam)?;
                    }
                }
                _ => {
                    let draft = EventService::agent_team_failed_draft(
                        self.operation_id.clone(),
                        self.options.team_id.clone(),
                        error,
                    );
                    if self.defer_terminal_publication {
                        self.event_service
                            .defer_terminal_draft(self.operation_id.clone(), draft)?;
                    } else {
                        self.event_service
                            .emit_committed_terminal_draft(draft, OperationKind::AgentTeam)?;
                    }
                }
            }
        }
        Ok(())
    }
}
