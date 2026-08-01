pub(crate) mod context;
pub(crate) mod runner;

use crate::operations::delegation::{
    DelegationAuthorizationDecision, PendingDelegationConfirmationQueue,
    PendingDelegationConfirmationState, delegation_lineage_for_request,
};
use crate::profiles::{ProfileId, ProfileKind, ProfileRegistry};
use crate::runtime::capability::{OperationCapabilitySnapshot, SessionWriteCapability};
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::control::OperationControl;
use crate::services::authorization::AuthorizationService;
use crate::services::event::EventService;
use crate::session::event::PersistedDelegationStatus;
use crate::session::id::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::session::service::{FinalizedSessionWrite, SessionPersistence, SessionService};
use context::{
    CodingDiagnostic, InternalPromptTurnOutcome, PromptTurnContext, PromptTurnIds,
    PromptTurnOptions,
};
use runner::PromptTurnRunner;
use tokio_util::sync::CancellationToken;

struct PromptOperation<'a> {
    persistence: &'a mut SessionPersistence,
    operation_control: &'a mut OperationControl,
    profile_registry: &'a ProfileRegistry,
    event_service: &'a EventService,
    pending_delegation_confirmations: &'a mut PendingDelegationConfirmationQueue,
    authorization_service: &'a AuthorizationService,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    persistence: &mut SessionPersistence,
    operation_control: &mut OperationControl,
    profile_registry: &ProfileRegistry,
    event_service: &EventService,
    pending_delegation_confirmations: &mut PendingDelegationConfirmationQueue,
    authorization_service: &AuthorizationService,
    options: PromptTurnOptions,
    snapshot: &OperationCapabilitySnapshot,
    cancellation: Option<CancellationToken>,
) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
    PromptOperation {
        persistence,
        operation_control,
        profile_registry,
        event_service,
        pending_delegation_confirmations,
        authorization_service,
    }
    .run_inner(options, snapshot, cancellation)
    .await
}

pub(crate) fn apply_default_agent_profile(
    persistence: &SessionPersistence,
    profile_registry: &ProfileRegistry,
    mut options: PromptTurnOptions,
) -> Result<PromptTurnOptions, CodingSessionError> {
    let profile_id = default_agent_profile_id(persistence);
    let mut diagnostics = Vec::new();
    let profile = match profile_registry.agent(profile_id.as_str()) {
        Some(profile) => profile,
        None => {
            diagnostics.push(CodingDiagnostic::warning(format!(
                "default agent profile {} could not be resolved; using built-in default profile",
                profile_id
            )));
            profile_registry
                .agent("default")
                .ok_or_else(|| CodingSessionError::Config {
                    message: "built-in default agent profile is not available".into(),
                })?
        }
    };
    options.apply_agent_profile(profile, profile_registry, diagnostics)?;
    Ok(options)
}

pub(crate) fn default_agent_profile_id(persistence: &SessionPersistence) -> ProfileId {
    match persistence {
        SessionPersistence::Persistent(session_service) => {
            session_service.current_default_agent_profile_id()
        }
        SessionPersistence::NonPersistent(state) => state.default_agent_profile_id.clone(),
    }
}

impl PromptOperation<'_> {
    async fn run_inner(
        &mut self,
        options: PromptTurnOptions,
        snapshot: &OperationCapabilitySnapshot,
        cancellation: Option<CancellationToken>,
    ) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
        if options.runtime().is_none() {
            return Err(CodingSessionError::Config {
                message: "prompt turn options do not include a runtime snapshot".into(),
            });
        }
        let mut context = self.prepare_prompt_context(options, snapshot, cancellation)?;
        self.install_delegation_executor(&mut context)?;
        let operation_id = context.operation_id().to_owned();
        let turn_id = context.turn_id().to_owned();

        self.event_service
            .emit_prompt_started(operation_id, turn_id);
        let turn_result: Result<InternalPromptTurnOutcome, CodingSessionError> =
            match PromptTurnRunner::new()?.run_typed(&mut context).await {
                Ok(_) => {
                    let session_id = context.session_id().map(str::to_owned);
                    context.finish_success(session_id, None)
                }
                Err(error) => match context.abort_reason() {
                    Some(reason) => Ok(context
                        .finish_abort(reason.to_owned(), context.session_id().map(str::to_owned))),
                    None => Ok(context.finish_failure(error)),
                },
            };
        let mut outcome = match turn_result {
            Ok(outcome) => outcome,
            Err(error) => match context.abort_reason() {
                Some(reason) => {
                    context.finish_abort(reason.to_owned(), context.session_id().map(str::to_owned))
                }
                None => context.finish_failure(error),
            },
        };
        if outcome.is_success() && !context.has_delegation_executor() {
            match context.authorize_delegation_requests(0) {
                Ok(decisions) => {
                    let decisions = decisions.to_vec();
                    let prompt_options = context.options().clone();
                    if let Err(error) = self
                        .execute_authorized_delegations(&mut context, &decisions, prompt_options)
                        .await
                    {
                        self.event_service.emit_diagnostic(
                            Some(context.operation_id().to_owned()),
                            format!("delegation execution failed: {error}"),
                        );
                    }
                    let deferred = context.take_deferred_pending_delegations();
                    crate::operations::delegation::confirmation::adopt_pending(
                        self.persistence,
                        self.pending_delegation_confirmations,
                        self.event_service,
                        deferred,
                    )?;
                }
                Err(error) => {
                    outcome = context.finish_failure(error);
                }
            }
        }
        let finalized = if let Some(error) = outcome.partial_commit_error().cloned() {
            // The transaction owner already froze itself as InDoubt at the
            // durable boundary. Discard the live handle without attempting a
            // second failure terminal that could mask the original uncertainty.
            let _ = context.take_transaction();
            SessionService::failed_prompt_transaction(context.operation_id().to_owned(), &error)
        } else {
            match self.finalize_prompt_transaction(&mut context, &outcome) {
                Ok(finalized) => finalized,
                Err(error) => {
                    outcome = context.finish_failure(error.clone());
                    SessionService::failed_prompt_transaction(
                        context.operation_id().to_owned(),
                        &error,
                    )
                }
            }
        };
        outcome.apply_success_session_write_metadata(
            finalized.session_id.clone(),
            finalized.leaf_id.clone(),
        );

        if !context.live_events_enabled() {
            self.event_service
                .emit_events_before_prompt_outcome(context.coding_events());
        }
        self.event_service.emit_session_write_events(&finalized);
        self.event_service.emit_prompt_diagnostics(&outcome);
        self.authorization_service
            .cancel_operation(context.operation_id(), "operation completed");
        Ok(outcome)
    }

    fn install_delegation_executor(
        &self,
        context: &mut PromptTurnContext,
    ) -> Result<(), CodingSessionError> {
        crate::operations::delegation::execution::install_tool_executor(
            context,
            self.profile_registry.clone(),
            self.event_service.clone(),
            self.operation_control.clone(),
            self.authorization_service.clone(),
            0,
            Vec::new(),
        )
    }

    async fn execute_authorized_delegations(
        &mut self,
        context: &mut PromptTurnContext,
        decisions: &[DelegationAuthorizationDecision],
        prompt_options: PromptTurnOptions,
    ) -> Result<(), CodingSessionError> {
        let parent_capability_snapshot = context.capability_snapshot().cloned();
        for decision in decisions {
            match decision {
                DelegationAuthorizationDecision::Approved {
                    request,
                    child_delegation_depth,
                } => {
                    self.event_service.emit_delegation_approved(request);
                    let outcome = match request.target_kind {
                        ProfileKind::Agent => {
                            crate::operations::delegation::execution::execute_agent(
                                self.profile_registry.clone(),
                                self.event_service.clone(),
                                self.operation_control.clone(),
                                request,
                                prompt_options.clone(),
                                *child_delegation_depth,
                                delegation_lineage_for_request(&[], request),
                                parent_capability_snapshot.clone(),
                                Some(self.authorization_service.clone()),
                                None,
                            )
                            .await
                        }
                        ProfileKind::Team => {
                            crate::operations::delegation::execution::execute_team(
                                self.profile_registry.clone(),
                                self.event_service.clone(),
                                self.operation_control.clone(),
                                request,
                                prompt_options.clone(),
                                *child_delegation_depth,
                                delegation_lineage_for_request(&[], request),
                                parent_capability_snapshot.clone(),
                                Some(self.authorization_service.clone()),
                                None,
                            )
                            .await
                        }
                    };
                    crate::operations::delegation::confirmation::adopt_pending(
                        self.persistence,
                        self.pending_delegation_confirmations,
                        self.event_service,
                        outcome.pending_confirmations,
                    )?;
                    match outcome.execution {
                        Ok(execution) => {
                            context.record_delegation_folded_update(
                                request,
                                PersistedDelegationStatus::Completed,
                                Some(execution.child_operation_id),
                                Some(execution.final_text),
                            )?;
                        }
                        Err(error) => {
                            context.record_delegation_folded_update(
                                request,
                                PersistedDelegationStatus::Failed,
                                None,
                                Some(error.to_string()),
                            )?;
                            return Err(error);
                        }
                    }
                }
                DelegationAuthorizationDecision::RequiresConfirmation {
                    request,
                    reason,
                    child_delegation_depth,
                } => {
                    context.record_delegation_folded_update(
                        request,
                        PersistedDelegationStatus::ConfirmationRequired,
                        None,
                        Some(reason.clone()),
                    )?;
                    let pending = PendingDelegationConfirmationState {
                        request: request.clone(),
                        prompt_options: prompt_options.clone(),
                        reason: reason.clone(),
                        requested_at: SystemClock.now_rfc3339(),
                        child_delegation_depth: *child_delegation_depth,
                        delegation_lineage: delegation_lineage_for_request(&[], request),
                    };
                    crate::operations::delegation::confirmation::queue_pending(
                        self.persistence,
                        self.pending_delegation_confirmations,
                        self.event_service,
                        pending,
                        true,
                    )?;
                }
                DelegationAuthorizationDecision::Rejected { request, reason } => {
                    self.event_service.emit_delegation_rejected(request, reason);
                    context.record_delegation_folded_update(
                        request,
                        PersistedDelegationStatus::Rejected,
                        None,
                        Some(reason.clone()),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn prepare_prompt_context(
        &mut self,
        options: PromptTurnOptions,
        snapshot: &OperationCapabilitySnapshot,
        cancellation: Option<CancellationToken>,
    ) -> Result<PromptTurnContext, CodingSessionError> {
        let event_service = self.event_service.clone();
        let prompt_control_receiver = self.operation_control.take_prompt_control_receiver();
        match &mut self.persistence {
            SessionPersistence::Persistent(session_service) => {
                let replay = session_service.replay()?;
                session_service.arm_auto_name_for_prompt(&replay);
                let transaction = session_service.begin_prompt_transaction_with_snapshot(snapshot);
                let operation_id = transaction.operation_id().to_owned();
                let turn_id = transaction.turn_id().to_owned();
                let mut context =
                    PromptTurnContext::new(PromptTurnIds::new(operation_id, turn_id), options);
                context.set_authorization_service(self.authorization_service.clone());
                context.set_authorization_event_writer(session_service.event_writer());
                context.set_session_id(session_service.session_id().to_owned());
                context.set_replay(replay);
                context.set_transaction(transaction);
                if let Some(receiver) = prompt_control_receiver {
                    context.set_prompt_control_receiver(receiver);
                }
                if let Some(cancellation) = cancellation.clone() {
                    context.set_operation_cancellation(cancellation);
                }
                context.enable_live_events(event_service);
                context.set_capability_snapshot(snapshot.clone());
                Ok(context)
            }
            SessionPersistence::NonPersistent(state) => {
                let mut ids = SystemIdGenerator;
                let mut context = PromptTurnContext::new(
                    PromptTurnIds::new(snapshot.operation_id.clone(), ids.next_turn_id()),
                    options,
                );
                context.set_authorization_service(self.authorization_service.clone());
                context
                    .set_non_persistent_session(state.runtime_id.clone(), state.transcript.clone());
                if let Some(receiver) = prompt_control_receiver {
                    context.set_prompt_control_receiver(receiver);
                }
                if let Some(cancellation) = cancellation {
                    context.set_operation_cancellation(cancellation);
                }
                context.enable_live_events(event_service);
                context.set_capability_snapshot(snapshot.clone());
                Ok(context)
            }
        }
    }

    fn finalize_prompt_transaction(
        &mut self,
        context: &mut PromptTurnContext,
        outcome: &InternalPromptTurnOutcome,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let operation_id = context.operation_id().to_owned();
        let transaction = context.take_transaction();
        match &mut self.persistence {
            SessionPersistence::Persistent(session_service) => {
                let snapshot = context.capability_snapshot().ok_or_else(|| {
                    CodingSessionError::UnsupportedCapability {
                        capability: "prompt session write requires operation capability snapshot"
                            .into(),
                    }
                })?;
                SessionWriteCapability::require(snapshot.session_write.as_ref())?;
                session_service.finalize_prompt_transaction(transaction, operation_id, outcome)
            }
            SessionPersistence::NonPersistent(state) => {
                Ok(state.finalize_prompt_transaction(context, outcome))
            }
        }
    }
}
