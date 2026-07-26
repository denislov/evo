use agent_core::api::resources::AgentResources;
use agent_core::api::tool::AgentTool;
use ai::api::model::{Model, ModelCost, ModelInput};

use crate::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::operations::prompt::context::PromptTurnOptions;

pub(crate) use crate::test_support::{EnvGuard, ProviderGuard};

pub(crate) fn assert_public_error(
    error: &CodingAgentPublicError,
    category: CodingAgentErrorCategory,
    code: &str,
    retryable: bool,
) {
    assert_eq!(error.category, category);
    assert_eq!(error.code(), code);
    assert_eq!(error.retryable, retryable);
    assert_eq!(error.context, CodingAgentErrorContext::None);
}

pub(crate) fn model(api: &str) -> Model {
    named_model("test-model", "Test Model", api)
}

fn fallback_model(api: &str) -> Model {
    named_model("fallback-model", "Fallback Model", api)
}

fn named_model(id: &str, name: &str, api: &str) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: api.into(),
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

pub(crate) fn prompt_options(
    cwd: &std::path::Path,
    api: &str,
    prompt: &str,
    tools: Vec<AgentTool>,
    max_turns: u32,
) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: fallback_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("Runtime fallback instructions.".into()),
        max_turns: Some(max_turns),
        tools,
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(cwd.to_path_buf())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text(prompt.into()),
    })
}
