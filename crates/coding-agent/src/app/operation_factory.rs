use std::fmt;

use agent_core::api::agent::{AgentResources, ThinkingLevel};
use ai::api::client::AiClient;
use ai_protocol::api::auth::ProviderAuthDiagnostic;
use ai_protocol::api::model::Model;
use tool_contract::api::definition::ToolExecutionMode;

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::embedding::CodingAgentThinkingLevel;
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::app::session::CodingAgentSessionBootstrap;
use crate::app::settings::CodingAgentQueueMode;
use crate::config::Settings;
use crate::operations::prompt::context::QueuedPromptInput;
use crate::profiles::ProfileId;
use crate::runtime::facade::{
    AgentInvocationOptions, AgentTeamOptions, BranchSummaryReusePolicy, CodingAgentOperation,
    PromptTurnOptions, SelfHealingEditModelRepairOptions, SelfHealingEditRequest,
};

/// Opaque product-owned factory for provider-neutral adapter operations.
///
/// Provider models, credentials, clients, executable tools, and complete
/// resources remain private. Product adapters retain this handle and submit
/// typed invocations instead of reconstructing lower-runtime options.
#[derive(Clone)]
pub struct CodingAgentOperationFactory {
    model: Model,
    api_key: Option<String>,
    auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    system_prompt: Option<String>,
    max_turns: Option<u32>,
    tools: Vec<std::sync::Arc<dyn tool_runtime::api::DynamicTool>>,
    register_builtins: bool,
    ai_client: Option<AiClient>,
    session: Option<SessionRunOptions>,
    thinking_level: Option<CodingAgentThinkingLevel>,
    tool_execution: Option<ToolExecutionMode>,
    resources: AgentResources,
    settings: Option<Settings>,
    initial_session_name: Option<String>,
    default_agent_profile_id: ProfileId,
}

impl fmt::Debug for CodingAgentOperationFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentOperationFactory")
            .field("model_id", &self.model.id)
            .field("persistent_session", &self.session.is_some())
            .field("default_agent_profile_id", &self.default_agent_profile_id)
            .finish_non_exhaustive()
    }
}

impl CodingAgentOperationFactory {
    #[allow(
        clippy::too_many_arguments,
        reason = "the private constructor captures the complete product runtime seed once"
    )]
    pub(crate) fn from_runtime_parts(
        model: Model,
        api_key: Option<String>,
        auth_diagnostics: Vec<ProviderAuthDiagnostic>,
        system_prompt: Option<String>,
        max_turns: Option<u32>,
        tools: Vec<std::sync::Arc<dyn tool_runtime::api::DynamicTool>>,
        register_builtins: bool,
        ai_client: Option<AiClient>,
        session: Option<SessionRunOptions>,
        thinking_level: Option<CodingAgentThinkingLevel>,
        tool_execution: Option<ToolExecutionMode>,
        resources: AgentResources,
        settings: Option<Settings>,
        initial_session_name: Option<String>,
        default_agent_profile_id: ProfileId,
    ) -> Self {
        Self {
            model,
            api_key,
            auth_diagnostics,
            system_prompt,
            max_turns,
            tools,
            register_builtins,
            ai_client,
            session,
            thinking_level,
            tool_execution,
            resources,
            settings,
            initial_session_name,
            default_agent_profile_id,
        }
    }

    pub fn selected_model_id(&self) -> &str {
        &self.model.id
    }

    pub(crate) fn selected_provider_id(&self) -> &str {
        &self.model.provider
    }

    pub fn default_agent_profile_id(&self) -> &ProfileId {
        &self.default_agent_profile_id
    }

    /// Bind private operation metadata to an opaque product session bootstrap.
    ///
    /// Session names and durable targets remain inaccessible to the adapter.
    pub fn bind_session_bootstrap(&mut self, bootstrap: &CodingAgentSessionBootstrap) {
        self.initial_session_name = bootstrap.initial_session_name().map(str::to_owned);
    }

    /// Construct a typed prompt operation without exposing its private runtime
    /// seed to the adapter.
    pub fn prompt_operation(
        &self,
        invocation: PromptInvocation,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> CodingAgentOperation {
        let mut options = self.default_prompt_options(invocation);
        if let Some(thinking_level) = thinking_level {
            options.thinking_level = Some(thinking_level.into());
        }
        CodingAgentOperation::Prompt(PromptTurnOptions::from_prompt_runtime_options(options))
    }

    pub(crate) fn prompt_operation_with_queued_inputs(
        &self,
        invocation: PromptInvocation,
        thinking_level: Option<CodingAgentThinkingLevel>,
        steering: Vec<QueuedPromptInput>,
        follow_up: Vec<QueuedPromptInput>,
    ) -> CodingAgentOperation {
        let CodingAgentOperation::Prompt(options) =
            self.prompt_operation(invocation, thinking_level)
        else {
            unreachable!("prompt operation factory returned a non-prompt operation");
        };
        CodingAgentOperation::Prompt(options.with_queued_inputs(steering, follow_up))
    }

    pub fn compact_operation(&self, custom_instructions: Option<String>) -> CodingAgentOperation {
        CodingAgentOperation::Compact(PromptTurnOptions::from_prompt_runtime_options(
            self.default_prompt_options(PromptInvocation::Compact {
                custom_instructions,
            }),
        ))
    }

    pub fn agent_invocation_operation(
        &self,
        profile_id: ProfileId,
        task: impl Into<String>,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> CodingAgentOperation {
        let task = task.into();
        let mut options = self.default_prompt_options(PromptInvocation::Text(task.clone()));
        if let Some(thinking_level) = thinking_level {
            options.thinking_level = Some(thinking_level.into());
        }
        CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new(
            profile_id,
            task,
            PromptTurnOptions::from_prompt_runtime_options(options),
        ))
    }

    pub fn team_invocation_operation(
        &self,
        team_id: ProfileId,
        task: impl Into<String>,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> CodingAgentOperation {
        let task = task.into();
        let mut options = self.default_prompt_options(PromptInvocation::Text(task.clone()));
        if let Some(thinking_level) = thinking_level {
            options.thinking_level = Some(thinking_level.into());
        }
        CodingAgentOperation::InvokeTeam(AgentTeamOptions::new(
            team_id,
            task,
            PromptTurnOptions::from_prompt_runtime_options(options),
        ))
    }

    pub fn branch_summary_operation(
        &self,
        source_leaf_id: impl Into<String>,
        target_leaf_id: impl Into<String>,
        custom_instructions: Option<String>,
        reuse: BranchSummaryReusePolicy,
    ) -> CodingAgentOperation {
        CodingAgentOperation::BranchSummary {
            options: PromptTurnOptions::from_prompt_runtime_options(
                self.default_prompt_options(PromptInvocation::Text(String::new())),
            ),
            source_leaf_id: source_leaf_id.into(),
            target_leaf_id: target_leaf_id.into(),
            custom_instructions,
            reuse,
        }
    }

    pub fn self_healing_edit_operation(
        &self,
        request: SelfHealingEditRequest,
    ) -> CodingAgentOperation {
        CodingAgentOperation::SelfHealingEdit(request)
    }

    pub fn fork_session_operation(&self, target_leaf_id: Option<String>) -> CodingAgentOperation {
        CodingAgentOperation::ForkSession { target_leaf_id }
    }

    pub fn model_repair_options(
        &self,
        thinking_level: Option<CodingAgentThinkingLevel>,
        max_attempts: usize,
    ) -> SelfHealingEditModelRepairOptions {
        let prompt = "repair self-healing edit".to_string();
        let options = PromptRuntimeOptions {
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            auth_diagnostics: self.auth_diagnostics.clone(),
            system_prompt: Some("Return only self-healing edit repair JSON.".to_string()),
            max_turns: Some(1),
            tools: self.tools.clone(),
            register_builtins: false,
            ai_client: self.ai_client.clone(),
            session: self.session.clone(),
            session_target: None,
            session_name: self.initial_session_name.clone(),
            thinking_level: thinking_level.map(ThinkingLevel::from),
            tool_execution: None,
            resources: AgentResources::default(),
            settings: self.settings.clone(),
            invocation: PromptInvocation::Text(prompt),
        };
        SelfHealingEditModelRepairOptions::new(PromptTurnOptions::from_prompt_runtime_options(
            options,
        ))
        .with_max_attempts(max_attempts)
    }

    pub(crate) fn replace_provider_runtime(
        &mut self,
        model: Model,
        api_key: Option<String>,
        auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    ) {
        self.model = model;
        self.api_key = api_key;
        self.auth_diagnostics = auth_diagnostics;
    }

    pub(crate) fn replace_auth(
        &mut self,
        api_key: Option<String>,
        auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    ) {
        self.api_key = api_key;
        self.auth_diagnostics = auth_diagnostics;
    }

    pub(crate) fn replace_settings(&mut self, settings: Settings) {
        self.settings = Some(settings);
    }

    pub(crate) fn configure_runtime_preferences(
        &mut self,
        thinking_level: CodingAgentThinkingLevel,
        steering_mode: CodingAgentQueueMode,
        follow_up_mode: CodingAgentQueueMode,
        auto_compaction_enabled: bool,
    ) {
        self.thinking_level = Some(thinking_level);
        if let Some(settings) = self.settings.as_mut() {
            settings.steering_mode = steering_mode.to_string();
            settings.follow_up_mode = follow_up_mode.to_string();
            settings.compaction.enabled = auto_compaction_enabled;
        }
    }

    fn default_prompt_options(&self, invocation: PromptInvocation) -> PromptRuntimeOptions {
        PromptRuntimeOptions {
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            auth_diagnostics: self.auth_diagnostics.clone(),
            system_prompt: self.system_prompt.clone(),
            max_turns: self.max_turns,
            tools: self.tools.clone(),
            register_builtins: self.register_builtins,
            ai_client: self.ai_client.clone(),
            session: self.session.clone(),
            session_target: None,
            session_name: self.initial_session_name.clone(),
            thinking_level: self.thinking_level.map(ThinkingLevel::from),
            tool_execution: self.tool_execution,
            resources: self.resources.clone(),
            settings: self.settings.clone(),
            invocation,
        }
    }
}
