use ai_protocol::api::conversation::AssistantMessage;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::PromptInvocation;
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::admission::OperationScheduler;
use crate::application::operation::control::OperationControl;
use crate::kernel::capability::ActorId;
use crate::kernel::control::PromptControlReceiver;
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
    InternalPromptTurnOutcome, PromptTurnContext, PromptTurnIds, PromptTurnOptions,
};
use crate::operations::prompt::runner::PromptTurnRunner;
use crate::platform::time::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::profiles::{AgentProfile, ProfileId, ProfileKind, ProfileRegistry};
use crate::public_error::CodingAgentPublicDiagnostic;
use crate::services::authorization::AuthorizationService;
use crate::services::event::EventService;

#[derive(Debug, Clone)]
pub struct AgentInvocationOptions {
    profile_id: ProfileId,
    task: String,
    prompt_options: PromptTurnOptions,
    delegation_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
}

impl AgentInvocationOptions {
    pub fn new(
        profile_id: impl Into<ProfileId>,
        task: impl Into<String>,
        prompt_options: PromptTurnOptions,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
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

    pub fn profile_id(&self) -> &ProfileId {
        &self.profile_id
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
pub struct AgentInvocationOutcome {
    pub operation_id: String,
    pub child_operation_id: String,
    pub turn_id: String,
    pub profile_id: ProfileId,
    pub final_text: String,
    pub final_message: AssistantMessage,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

pub(crate) struct AgentInvocationRunner;

impl AgentInvocationRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut AgentInvocationContext,
        cancellation: Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        let result: Result<(), CodingSessionError> = async {
            Self::check_cancellation(&cancellation)?;
            ctx.start_agent_invocation()?;
            Self::check_cancellation(&cancellation)?;
            ctx.resolve_agent_profile()?;
            ctx.provision_child_workspace(cancellation.as_ref()).await?;
            Self::check_cancellation(&cancellation)?;
            ctx.prepare_child_prompt()?;
            if let Some(token) = cancellation.as_ref()
                && token.is_cancelled()
            {
                ctx.propagate_child_cancellation(token.clone());
            }
            ctx.run_child_agent().await?;
            Self::check_cancellation(&cancellation)?;
            ctx.finalize_agent_invocation()?;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            ctx.record_failure_terminal(error)?;
            if let Err(error) = ctx.release_child_worktree() {
                ctx.event_service
                    .emit_diagnostic(Some(ctx.operation_id.clone()), error.to_string())?;
            }
        } else {
            match ctx.promote_child_worktree() {
                Ok(Some(worktree_id)) => {
                    ctx.event_service.emit_merge_proposal_created(
                        ctx.operation_id.clone(),
                        worktree_id,
                        ctx.child_operation_id.clone(),
                    )?;
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = ctx.release_child_worktree();
                    ctx.event_service
                        .emit_diagnostic(Some(ctx.operation_id.clone()), error.to_string())?;
                }
            }
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

pub(crate) struct AgentInvocationContext {
    options: AgentInvocationOptions,
    registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    operation_id: String,
    child_operation_id: String,
    turn_id: String,
    profile: Option<AgentProfile>,
    child_context: Option<PromptTurnContext>,
    prompt_control_receiver: Option<PromptControlReceiver>,
    prompt_outcome: Option<InternalPromptTurnOutcome>,
    parent_capability_snapshot: Option<OperationCapabilitySnapshot>,
    child_capability_snapshot: Option<OperationCapabilitySnapshot>,
    child_admission: Option<crate::application::operation::permit::OperationPermit>,
    child_workspace_binding: Option<ChildWorkspaceBinding>,
    child_worktree: Option<ChildWorktreeLease>,
    pending_delegation_confirmations: Vec<PendingDelegationConfirmationState>,
    failure_terminal_recorded: bool,
    defer_terminal_publication: bool,
    authorization_service: Option<AuthorizationService>,
}

impl AgentInvocationContext {
    pub(crate) fn new(
        options: AgentInvocationOptions,
        registry: ProfileRegistry,
        event_service: EventService,
        operation_control: OperationControl,
        operation_id: String,
    ) -> Self {
        let mut ids = SystemIdGenerator;
        Self {
            options,
            registry,
            event_service,
            operation_control,
            operation_id,
            child_operation_id: OperationScheduler::allocate_child_operation_id(),
            turn_id: ids.next_turn_id(),
            profile: None,
            child_context: None,
            prompt_control_receiver: None,
            prompt_outcome: None,
            parent_capability_snapshot: None,
            child_capability_snapshot: None,
            child_admission: None,
            child_workspace_binding: None,
            child_worktree: None,
            pending_delegation_confirmations: Vec::new(),
            failure_terminal_recorded: false,
            defer_terminal_publication: false,
            authorization_service: None,
        }
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
            EventService::agent_invocation_aborted_draft(
                self.operation_id.clone(),
                self.child_operation_id.clone(),
                self.options.profile_id.clone(),
                error.to_string(),
            )
        } else {
            EventService::agent_invocation_failed_draft(
                self.operation_id.clone(),
                self.child_operation_id.clone(),
                self.options.profile_id.clone(),
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

    pub(crate) fn set_prompt_control_receiver(&mut self, receiver: PromptControlReceiver) {
        self.prompt_control_receiver = Some(receiver);
    }

    #[cfg(test)]
    pub(crate) fn child_capability_snapshot(&self) -> Option<&OperationCapabilitySnapshot> {
        self.child_capability_snapshot.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn child_context(&self) -> Option<&PromptTurnContext> {
        self.child_context.as_ref()
    }

    pub(crate) fn finish_success(&self) -> Result<AgentInvocationOutcome, CodingSessionError> {
        let outcome = self
            .prompt_outcome
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "agent invocation completed without child prompt outcome".into(),
            })?;
        match outcome {
            InternalPromptTurnOutcome::Success {
                turn_id,
                final_text,
                final_message,
                diagnostics,
                ..
            } => Ok(AgentInvocationOutcome {
                operation_id: self.operation_id.clone(),
                child_operation_id: self.child_operation_id.clone(),
                turn_id: turn_id.clone(),
                profile_id: self.options.profile_id.clone(),
                final_text: final_text.clone(),
                final_message: final_message.clone(),
                diagnostics: CodingAgentPublicDiagnostic::from_runtime_diagnostics(
                    diagnostics,
                    Some(&self.child_operation_id),
                ),
            }),
            InternalPromptTurnOutcome::Aborted { reason, .. } => Err(CodingSessionError::Session {
                message: format!("agent invocation aborted: {reason}"),
            }),
            InternalPromptTurnOutcome::Failed { error, .. } => Err(error.clone()),
        }
    }

    fn start_agent_invocation(&mut self) -> Result<(), CodingSessionError> {
        if self.options.task.trim().is_empty() {
            return Err(CodingSessionError::Input {
                message: "agent invocation requires a non-empty task".into(),
            });
        }
        self.event_service.emit_agent_invocation_started(
            self.operation_id.clone(),
            self.child_operation_id.clone(),
            self.options.profile_id.clone(),
            self.options.task.clone(),
        )?;
        Ok(())
    }

    fn resolve_agent_profile(&mut self) -> Result<(), CodingSessionError> {
        let profile = self
            .registry
            .agent(self.options.profile_id.as_str())
            .cloned()
            .ok_or_else(|| CodingSessionError::Input {
                message: format!("Unknown agent profile: {}", self.options.profile_id),
            })?;
        self.profile = Some(profile);
        Ok(())
    }

    /// Provision the child's isolated workspace before admission.
    ///
    /// Write-capable children get a managed worktree bound to their own
    /// capability snapshot; read-only children share the parent read path;
    /// projectless children carry no workspace. The lease is released on every
    /// terminal path of [`AgentInvocationRunner::run_typed`].
    async fn provision_child_workspace(
        &mut self,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        let parent = self.parent_capability_snapshot.as_ref().ok_or_else(|| {
            CodingSessionError::UnsupportedCapability {
                capability: "agent child operation requires an admitted parent capability snapshot"
                    .into(),
            }
        })?;
        let profile = self
            .profile
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "agent invocation cannot provision a workspace before profile resolution"
                    .into(),
            })?;
        let policy = ChildWorkspacePolicy::decide(parent, profile);
        let Some(lease) = bind_child_workspace(
            &self.operation_control,
            parent,
            &self.child_operation_id,
            None,
            cancellation.unwrap_or(&CancellationToken::new()),
            policy,
        )
        .await?
        else {
            self.child_workspace_binding = Some(match policy {
                ChildWorkspacePolicy::Projectless => ChildWorkspaceBinding::None,
                ChildWorkspacePolicy::ReadOnlyShared => ChildWorkspaceBinding::ReadOnlyShared,
                ChildWorkspacePolicy::Managed => {
                    unreachable!("managed policy always returns a lease or fails closed")
                }
            });
            return Ok(());
        };
        let handle = lease.handle()?;
        self.child_worktree = Some(lease);
        self.child_workspace_binding = Some(ChildWorkspaceBinding::Managed(handle));
        Ok(())
    }

    fn release_child_worktree(&mut self) -> Result<(), CodingSessionError> {
        if let Some(lease) = self.child_worktree.as_mut() {
            lease.release()?;
        }
        Ok(())
    }

    /// Keep a successful child's worktree for review and merge.
    ///
    /// Returns the worktree id when a managed worktree was provisioned, `None`
    /// for projectless/read-only children that never had one.
    fn promote_child_worktree(&mut self) -> Result<Option<String>, CodingSessionError> {
        if let Some(lease) = self.child_worktree.as_mut() {
            lease.promote_to_merge_pending()?;
            Ok(Some(lease.record_id().to_owned()))
        } else {
            Ok(None)
        }
    }

    fn prepare_child_prompt(&mut self) -> Result<(), CodingSessionError> {
        let profile = self
            .profile
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "agent invocation cannot prepare child prompt before profile resolution"
                    .into(),
            })?;
        let mut prompt_options = self.options.prompt_options.clone();
        prompt_options.set_invocation(PromptInvocation::Text(self.options.task.clone()));
        if self.options.delegation_depth > 0 {
            prompt_options.apply_delegated_agent_profile(profile, &self.registry, Vec::new())?;
        } else {
            prompt_options.apply_agent_profile(profile, &self.registry, Vec::new())?;
        }
        if prompt_options.runtime().is_none() {
            return Err(CodingSessionError::Config {
                message: "agent invocation options do not include a runtime snapshot".into(),
            });
        }
        let parent = self.parent_capability_snapshot.as_ref().ok_or_else(|| {
            CodingSessionError::UnsupportedCapability {
                capability: "agent child operation requires an admitted parent capability snapshot"
                    .into(),
            }
        })?;
        let binding =
            self.child_workspace_binding
                .as_ref()
                .ok_or_else(|| CodingSessionError::Session {
                    message:
                        "agent invocation cannot prepare child prompt before workspace provisioning"
                            .into(),
                })?;
        if let Some(handle) = binding.as_managed()
            && let Some(lease) = self.child_worktree.as_ref()
        {
            prompt_options.bind_child_workspace(lease.root().to_path_buf(), handle.clone())?;
        }
        let mut child_context = PromptTurnContext::new(
            PromptTurnIds::new(self.child_operation_id.clone(), self.turn_id.clone()),
            prompt_options,
        );
        child_context.set_non_persistent_session(
            format!("agent_invocation_{}", self.child_operation_id),
            Vec::new(),
        );
        if let Some(receiver) = self.prompt_control_receiver.take() {
            child_context.set_prompt_control_receiver(receiver);
        }
        child_context.enable_live_events(self.event_service.clone());
        if let Some(service) = self.authorization_service.clone() {
            child_context.set_authorization_service(service);
        }
        let capability_snapshot = capability_snapshot_for_delegated_profile(
            parent,
            self.child_operation_id.clone(),
            profile,
            ActorId::ChildOperation(parent.operation_id.clone()),
            binding.clone(),
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
        self.child_admission = Some(child_admission);
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
        self.child_context = Some(child_context);
        Ok(())
    }

    async fn run_child_agent(&mut self) -> Result<(), CodingSessionError> {
        let mut finished_outcome = None;
        let child_delegations = {
            let child_context =
                self.child_context
                    .as_mut()
                    .ok_or_else(|| CodingSessionError::Session {
                        message: "agent invocation cannot run before child prompt preparation"
                            .into(),
                    })?;
            match PromptTurnRunner::new()?.run_typed(child_context).await {
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
            }
        };

        let Some((decisions, prompt_options, runtime_id)) = child_delegations else {
            self.prompt_outcome = finished_outcome;
            return Ok(());
        };
        self.child_capability_snapshot = self
            .child_context
            .as_ref()
            .and_then(|context| context.capability_snapshot().cloned());
        if let Err(error) = self
            .execute_authorized_delegations(&decisions, prompt_options)
            .await
        {
            self.event_service.emit_diagnostic(
                Some(self.child_operation_id.clone()),
                format!("delegation execution failed: {error}"),
            )?;
        }
        let child_context =
            self.child_context
                .as_ref()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "agent invocation completed without child prompt context".into(),
                })?;
        self.prompt_outcome = Some(child_context.finish_success(runtime_id, None)?);
        Ok(())
    }

    fn propagate_child_cancellation(&mut self, cancellation: CancellationToken) {
        if let Some(child_context) = self.child_context.as_mut() {
            child_context.set_operation_cancellation(cancellation);
        }
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
        request: &crate::operations::prompt::context::DelegationRequest,
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
        ))
        .await;
        self.pending_delegation_confirmations
            .extend(outcome.pending_confirmations);
        outcome.execution.map(|_| ())
    }

    async fn execute_approved_team_delegation(
        &mut self,
        request: &crate::operations::prompt::context::DelegationRequest,
        prompt_options: PromptTurnOptions,
        child_delegation_depth: usize,
    ) -> Result<(), CodingSessionError> {
        let outcome = crate::operations::delegation::execution::execute_team(
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
        )
        .await;
        self.pending_delegation_confirmations
            .extend(outcome.pending_confirmations);
        outcome.execution.map(|_| ())
    }

    fn finalize_agent_invocation(&mut self) -> Result<(), CodingSessionError> {
        let outcome = self
            .prompt_outcome
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "agent invocation cannot finalize without child prompt outcome".into(),
            })?;
        match outcome {
            InternalPromptTurnOutcome::Success {
                turn_id,
                final_text,
                diagnostics,
                ..
            } => {
                for diagnostic in diagnostics {
                    self.event_service.emit_diagnostic(
                        Some(self.operation_id.clone()),
                        diagnostic.message.clone(),
                    )?;
                }
                self.event_service
                    .emit_prompt_completed(self.child_operation_id.clone(), turn_id.clone())?;
                let draft = EventService::agent_invocation_completed_draft(
                    self.operation_id.clone(),
                    self.child_operation_id.clone(),
                    self.options.profile_id.clone(),
                    final_text.clone(),
                );
                if self.defer_terminal_publication {
                    self.event_service
                        .defer_terminal_draft(self.operation_id.clone(), draft)?;
                } else {
                    self.event_service
                        .emit_committed_terminal_draft(draft, OperationKind::AgentInvocation)?;
                }
                self.child_admission.take();
                Ok(())
            }
            InternalPromptTurnOutcome::Aborted { reason, .. } => {
                let error = CodingSessionError::Cancelled;
                self.failure_terminal_recorded = true;
                self.event_service
                    .emit_prompt_aborted(self.child_operation_id.clone(), reason.clone())?;
                let draft = EventService::agent_invocation_aborted_draft(
                    self.operation_id.clone(),
                    self.child_operation_id.clone(),
                    self.options.profile_id.clone(),
                    reason.clone(),
                );
                if self.defer_terminal_publication {
                    self.event_service
                        .defer_terminal_draft(self.operation_id.clone(), draft)?;
                } else {
                    self.event_service
                        .emit_committed_terminal_draft(draft, OperationKind::AgentInvocation)?;
                }
                self.child_admission.take();
                Err(error)
            }
            InternalPromptTurnOutcome::Failed { error, .. } => {
                let error = error.clone();
                self.failure_terminal_recorded = true;
                self.event_service
                    .emit_prompt_failed(self.child_operation_id.clone(), error.clone())?;
                let draft = EventService::agent_invocation_failed_draft(
                    self.operation_id.clone(),
                    self.child_operation_id.clone(),
                    self.options.profile_id.clone(),
                    &error,
                );
                if self.defer_terminal_publication {
                    self.event_service
                        .defer_terminal_draft(self.operation_id.clone(), draft)?;
                } else {
                    self.event_service
                        .emit_committed_terminal_draft(draft, OperationKind::AgentInvocation)?;
                }
                self.child_admission.take();
                Err(error)
            }
        }
    }

    fn record_failure_terminal(
        &mut self,
        error: &CodingSessionError,
    ) -> Result<(), CodingSessionError> {
        if self.child_admission.is_some() {
            if *error == CodingSessionError::Cancelled {
                self.event_service
                    .emit_prompt_aborted(self.child_operation_id.clone(), error.to_string())?;
            } else {
                self.event_service
                    .emit_prompt_failed(self.child_operation_id.clone(), error.clone())?;
            }
        }
        self.child_admission.take();
        if !self.failure_terminal_recorded {
            self.failure_terminal_recorded = true;
            if *error == CodingSessionError::Cancelled {
                let draft = EventService::agent_invocation_aborted_draft(
                    self.operation_id.clone(),
                    self.child_operation_id.clone(),
                    self.options.profile_id.clone(),
                    error.to_string(),
                );
                if self.defer_terminal_publication {
                    self.event_service
                        .defer_terminal_draft(self.operation_id.clone(), draft)?;
                } else {
                    self.event_service
                        .emit_committed_terminal_draft(draft, OperationKind::AgentInvocation)?;
                }
            } else {
                let draft = EventService::agent_invocation_failed_draft(
                    self.operation_id.clone(),
                    self.child_operation_id.clone(),
                    self.options.profile_id.clone(),
                    error,
                );
                if self.defer_terminal_publication {
                    self.event_service
                        .defer_terminal_draft(self.operation_id.clone(), draft)?;
                } else {
                    self.event_service
                        .emit_committed_terminal_draft(draft, OperationKind::AgentInvocation)?;
                }
            }
        }
        Ok(())
    }
}
