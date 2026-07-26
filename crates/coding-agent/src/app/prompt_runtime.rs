use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::session::ResolvedSessionTarget;
use agent_core::api::agent::{AgentResources, ThinkingLevel};
use agent_core::api::tool::{AgentTool, ToolExecutionMode};
use ai::api::auth::ProviderAuthDiagnostic;
use ai::api::client::AiClient;
use ai::api::conversation::{AssistantMessage, ContentBlock};
use ai::api::model::Model;

pub(crate) struct PromptRuntimeOptions {
    pub model: Model,
    pub api_key: Option<String>,
    pub auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    pub system_prompt: Option<String>,
    pub max_turns: Option<u32>,
    pub tools: Vec<AgentTool>,
    pub register_builtins: bool,
    pub ai_client: Option<AiClient>,
    pub session: Option<SessionRunOptions>,
    pub session_target: Option<ResolvedSessionTarget>,
    pub session_name: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub resources: AgentResources,
    pub settings: Option<crate::config::Settings>,
    pub invocation: PromptInvocation,
}

pub(crate) fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
