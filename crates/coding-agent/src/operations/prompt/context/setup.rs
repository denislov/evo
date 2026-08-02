use super::*;

impl PromptTurnContext {
    pub(crate) fn new(ids: PromptTurnIds, options: PromptTurnOptions) -> Self {
        Self {
            ids,
            options,
            request_resolved: false,
            runtime: None,
            prepared_input: None,
            loaded_resources: None,
            replay: None,
            session_id: None,
            non_persistent_runtime_id: None,
            agent: None,
            transaction: None,
            final_message: None,
            completion_recorded: false,
            coding_events: Vec::new(),
            delegation_requests: Vec::new(),
            delegation_authorization_decisions: Vec::new(),
            assistant_session_message_id: None,
            completed_assistant_session_message_id: None,
            reasoning_duration: ReasoningDurationTracker::default(),
            live_event_service: None,
            prompt_control_receiver: None,
            operation_cancellation: None,
            authorization_service: None,
            authorization_event_writer: None,
            tool_session_call_ids: HashMap::new(),
            diagnostics: Vec::new(),
            requested_abort_reason: None,
            capability_snapshot: None,
            delegation_executor: None,
            deferred_pending_delegations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.ids.operation_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.ids.turn_id
    }

    pub(crate) fn options(&self) -> &PromptTurnOptions {
        &self.options
    }

    pub(crate) fn set_authorization_service(&mut self, service: AuthorizationService) {
        self.authorization_service = Some(service);
    }

    pub(crate) fn set_authorization_event_writer(&mut self, writer: SessionEventWriter) {
        self.authorization_event_writer = Some(Arc::new(writer));
    }

    pub(crate) fn authorization_hook_context(&self) -> Option<AuthorizationHookContext> {
        let service = self.authorization_service.as_ref()?;
        let capability_snapshot = self.capability_snapshot.as_ref()?;
        Some(AuthorizationHookContext {
            service: service.clone(),
            turn_id: self.turn_id().to_owned(),
            capability_snapshot: capability_snapshot.clone(),
            event_writer: self.authorization_event_writer.clone(),
        })
    }

    pub(crate) fn set_capability_snapshot(&mut self, snapshot: OperationCapabilitySnapshot) {
        self.capability_snapshot = Some(snapshot);
    }

    pub(crate) fn set_delegation_executor(&mut self, executor: DelegationToolExecutor) {
        self.delegation_executor = Some(executor);
    }

    pub(crate) fn delegation_executor(&self) -> Option<DelegationToolExecutor> {
        self.delegation_executor.clone()
    }

    pub(crate) fn has_delegation_executor(&self) -> bool {
        self.delegation_executor.is_some()
    }

    pub(crate) fn deferred_pending_delegations(
        &self,
    ) -> Arc<Mutex<Vec<PendingDelegationConfirmationState>>> {
        self.deferred_pending_delegations.clone()
    }

    pub(crate) fn take_deferred_pending_delegations(
        &self,
    ) -> Result<Vec<PendingDelegationConfirmationState>, CodingSessionError> {
        Ok(self
            .deferred_pending_delegations
            .lock_resource("deferred delegation queue")?
            .drain(..)
            .collect())
    }

    pub(crate) fn capability_snapshot(&self) -> Option<&OperationCapabilitySnapshot> {
        self.capability_snapshot.as_ref()
    }

    pub(crate) fn set_runtime(&mut self, runtime: RuntimeSnapshot) {
        self.runtime = Some(runtime);
    }

    pub(crate) fn resolve_request(&mut self) -> Result<(), CodingSessionError> {
        if self.request_resolved {
            return Ok(());
        }
        match self.options.invocation() {
            PromptInvocation::Text(text) if text.is_empty() => {
                return Err(CodingSessionError::Input {
                    message: "prompt turn requires non-empty text input".into(),
                });
            }
            PromptInvocation::Content(content) if content.is_empty() => {
                return Err(CodingSessionError::Input {
                    message: "prompt turn requires non-empty content input".into(),
                });
            }
            PromptInvocation::Compact { .. } => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: "manual compaction in PromptTurnRunner".into(),
                });
            }
            PromptInvocation::Text(_)
            | PromptInvocation::Content(_)
            | PromptInvocation::Skill { .. }
            | PromptInvocation::PromptTemplate { .. } => {}
        }
        if self.options.runtime().is_none() {
            return Err(CodingSessionError::Config {
                message: "prompt turn options do not include a runtime snapshot".into(),
            });
        }
        self.request_resolved = true;
        Ok(())
    }

    pub(crate) fn resolve_runtime_from_options(&mut self) -> Result<(), CodingSessionError> {
        if self.runtime.is_some() {
            return Ok(());
        }
        self.require_resolved_request("resolve runtime")?;
        let runtime =
            self.options
                .runtime()
                .cloned()
                .ok_or_else(|| CodingSessionError::Config {
                    message: "prompt turn options do not include a runtime snapshot".into(),
                })?;
        self.set_runtime(runtime);
        Ok(())
    }

    pub(crate) fn runtime(&self) -> Option<&RuntimeSnapshot> {
        self.runtime.as_ref()
    }

    pub(crate) fn prepare_input(&mut self) -> Result<(), CodingSessionError> {
        if self.prepared_input.is_some() {
            return Ok(());
        }
        self.require_resolved_request("prepare input")?;
        self.prepared_input = Some(persisted_content_blocks_from_invocation(
            self.options.invocation(),
        )?);
        Ok(())
    }

    pub(crate) fn load_resources_from_runtime(&mut self) -> Result<(), CodingSessionError> {
        if self.loaded_resources.is_some() {
            return Ok(());
        }
        let resources = self
            .runtime
            .as_ref()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn cannot load resources without a runtime snapshot".into(),
            })?
            .resources()
            .clone();
        self.loaded_resources = Some(resources);
        Ok(())
    }

    pub(crate) fn loaded_resources(&self) -> Option<&AgentResources> {
        self.loaded_resources.as_ref()
    }

    pub(crate) fn set_replay(&mut self, replay: SessionReplay) {
        self.replay = Some(replay);
    }

    pub(crate) fn replay(&self) -> Option<&SessionReplay> {
        self.replay.as_ref()
    }

    pub(crate) fn set_non_persistent_session(
        &mut self,
        runtime_id: impl Into<String>,
        transcript: Vec<TranscriptItem>,
    ) {
        let runtime_id = runtime_id.into();
        self.non_persistent_runtime_id = Some(runtime_id.clone());
        self.session_id = None;
        self.transaction = None;
        self.replay = Some(SessionReplay {
            session_id: runtime_id,
            committed_through_session_sequence: 0,
            cwd: None,
            active_leaf_id: None,
            leaves: Vec::new(),
            tree_labels: Default::default(),
            transcript,
            diagnostics: Vec::new(),
            pending_delegation_confirmations: Vec::new(),
            pending_tool_authorizations: Vec::new(),
            usage: Default::default(),
            operation_statuses: Default::default(),
        });
    }

    pub(crate) fn non_persistent_runtime_id(&self) -> Option<&str> {
        self.non_persistent_runtime_id.as_deref()
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub(crate) fn set_session_id(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
        self.non_persistent_runtime_id = None;
    }

    pub(crate) fn set_agent(&mut self, agent: Agent) {
        self.agent = Some(agent);
    }

    pub(crate) fn agent(&self) -> Option<&Agent> {
        self.agent.as_ref()
    }

    pub(crate) fn set_transaction(&mut self, transaction: PromptTurnTransaction) {
        self.transaction = Some(transaction);
    }

    pub(crate) fn has_active_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    pub(crate) fn take_transaction(&mut self) -> Option<PromptTurnTransaction> {
        self.transaction.take()
    }

    pub(crate) fn enable_live_events(&mut self, event_service: EventService) {
        self.live_event_service = Some(event_service);
    }

    pub(crate) fn live_events_enabled(&self) -> bool {
        self.live_event_service.is_some()
    }

    pub(crate) fn set_prompt_control_receiver(&mut self, receiver: PromptControlReceiver) {
        self.prompt_control_receiver = Some(receiver);
    }

    pub(crate) fn take_prompt_control_receiver(&mut self) -> Option<PromptControlReceiver> {
        self.prompt_control_receiver.take()
    }

    pub(crate) fn set_operation_cancellation(&mut self, cancellation: CancellationToken) {
        self.operation_cancellation = Some(cancellation);
    }

    pub(crate) fn operation_cancellation(&self) -> Option<CancellationToken> {
        self.operation_cancellation.clone()
    }
}
