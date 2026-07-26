use super::*;
use crate::session::id::{Clock, SystemClock};

impl CodingAgentSession {
    #[cfg(test)]
    pub(crate) fn arm_append_events_failure_for_tests(&self, successful_calls: usize) {
        self.persistent_session_service()
            .fail_store_after_for_tests(
                crate::session::repository::StoreFailurePoint::AppendEvents,
                successful_calls,
            );
    }

    #[cfg(test)]
    pub(crate) fn arm_update_manifest_failure_for_tests(&self, successful_calls: usize) {
        self.persistent_session_service()
            .fail_store_after_for_tests(
                crate::session::repository::StoreFailurePoint::UpdateManifest,
                successful_calls,
            );
    }

    #[cfg(test)]
    pub(crate) fn queue_pending_delegation_for_tests(
        &mut self,
        operation_id: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) {
        let prompt = "delegated task";
        let prompt_options = PromptTurnOptions::from_prompt_runtime_options(
            crate::app::prompt_runtime::PromptRuntimeOptions {
                model: ai::api::model::Model {
                    id: "test-model".into(),
                    name: "Test Model".into(),
                    api: "interactive-pending-delegation-fixture".into(),
                    provider: "test".into(),
                    base_url: String::new(),
                    reasoning: false,
                    thinking_level_map: None,
                    input: vec![ai::api::model::ModelInput::Text],
                    cost: ai::api::model::ModelCost::default(),
                    context_window: 0,
                    max_tokens: 0,
                    headers: None,
                    compat: None,
                },
                api_key: None,
                auth_diagnostics: Vec::new(),
                system_prompt: Some("system".into()),
                max_turns: Some(2),
                tools: Vec::new(),
                register_builtins: false,
                ai_client: None,
                session: Some(crate::app::bootstrap::SessionRunOptions::disabled(
                    ".".into(),
                )),
                session_target: None,
                session_name: None,
                thinking_level: None,
                tool_execution: None,
                resources: agent_core::api::resources::AgentResources::default(),
                settings: None,
                invocation: crate::app::bootstrap::PromptInvocation::Text(prompt.into()),
            },
        );
        let pending = PendingDelegationConfirmationState {
            request: crate::operations::prompt::context::DelegationRequest {
                operation_id: operation_id.into(),
                turn_id: "turn_interactive_fixture".into(),
                tool_call_id: tool_call_id.into(),
                requesting_profile_id: ProfileId::from("parent"),
                target_kind: ProfileKind::Agent,
                target_id: ProfileId::from("default"),
                task: "delegated task".into(),
            },
            prompt_options,
            reason: "requires confirmation".into(),
            requested_at: SystemClock.now_rfc3339(),
            child_delegation_depth: 1,
            delegation_lineage: Vec::new(),
        };
        crate::operations::delegation::confirmation::queue_pending(
            &mut self.runtime_host.session_coordinator.persistence,
            &mut self
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations,
            &self.runtime_host.event_hub.service,
            pending,
            true,
        )
        .expect("pending delegation fixture requires a persistent session");
    }

    #[cfg(test)]
    pub(super) fn persistent_session_service(&self) -> &SessionService {
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => session_service,
            SessionPersistence::NonPersistent(_) => {
                panic!("expected persistent coding agent session")
            }
        }
    }

    #[cfg(test)]
    pub(super) fn current_capability_generation_for_tests(
        &self,
    ) -> crate::runtime::capability::CapabilityGeneration {
        self.runtime_host
            .operation_supervisor
            .capabilities
            .current_generation()
    }
}
