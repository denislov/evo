use crate::app::error::ApplicationError;
use crate::app::invocation::CodingAgentInvocationOptions;
use agent_core::api::agent::{
    AgentConfig, AgentResources, CompactionConfig, CompactionSettings, ThinkingLevel,
};
use agent_core::api::tool::{AgentTool, ToolExecutionMode};
use ai::api::auth::ProviderAuthDiagnostic;
use ai::api::client::AiClient;
use ai::api::model::Model;
use ai::api::stream::StreamOptions;
use std::path::PathBuf;

use crate::workspace::CodingAgentResolvedWorkspace;

pub const DEFAULT_MODEL_ID: &str = "claude-sonnet-4-5";

/// Default system prompt mirroring TS `buildSystemPrompt` in
/// `pi/packages/coding-agent/src/core/system-prompt.ts`.  Built-in tools
/// (read, bash, edit, write) are described with one-line snippets;
/// guidelines cover file exploration, conciseness, and path visibility.
/// The pi documentation section mirrors TS self-referencing doc paths.
pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an expert coding assistant operating inside evo, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
- read: Read file contents
- bash: Execute shell commands
- edit: Precise file editing
- write: Create or overwrite files

In addition to the tools above, you may have access to other custom tools depending on the project.

Guidelines:
- Use bash for file operations like ls, rg, find
- Be concise in your responses
- Show file paths clearly when working with files"#;

#[derive(Clone, Debug)]
pub enum SessionMode {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct SessionRunOptions {
    pub mode: SessionMode,
    pub cwd: PathBuf,
    pub session_dir: Option<PathBuf>,
    pub(crate) workspace: Option<CodingAgentResolvedWorkspace>,
}

impl SessionRunOptions {
    pub fn disabled(cwd: PathBuf) -> Self {
        Self {
            mode: SessionMode::Disabled,
            cwd,
            session_dir: None,
            workspace: None,
        }
    }

    pub fn enabled(cwd: PathBuf) -> Self {
        Self {
            mode: SessionMode::Enabled,
            cwd,
            session_dir: None,
            workspace: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ApplicationRunOptions {
    pub model_override: Option<Model>,
    pub tools: Vec<AgentTool>,
    pub register_builtins: bool,
    pub global_config_only: bool,
    pub ai_client: Option<AiClient>,
    pub session: SessionRunOptions,
}

impl Default for ApplicationRunOptions {
    fn default() -> Self {
        Self {
            model_override: None,
            tools: Vec::new(),
            register_builtins: true,
            global_config_only: false,
            ai_client: None,
            session: SessionRunOptions::disabled(PathBuf::from(".")),
        }
    }
}

pub fn select_model(
    args: &CodingAgentInvocationOptions,
    default_provider: Option<&str>,
    default_model: Option<&str>,
    model_override: Option<Model>,
) -> Result<Model, ApplicationError> {
    let effective_provider = args.provider.as_deref().or(default_provider);
    if let Some(models) = args.model_rotation.as_deref() {
        let rotation = crate::app::model_selection::parse_model_rotation(models)?;
        let mut candidates = ai::api::model::all_models().to_vec();
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        if let Some(provider) = effective_provider {
            candidates.retain(|model| model.provider == provider);
        }
        if let Some(model) = candidates
            .into_iter()
            .find(|model| rotation.matches(&model.id) || rotation.matches(&model.name))
        {
            return Ok(model);
        }
        return Err(ApplicationError::UnknownModel(models.to_string()));
    }

    if let Some(model_id) = &args.model {
        let model = ai::api::model::lookup_model(model_id)
            .ok_or_else(|| ApplicationError::UnknownModel(model_id.clone()))?;
        if let Some(provider) = effective_provider
            && model.provider != provider
        {
            return Err(ApplicationError::UnknownModel(format!(
                "{provider}/{model_id}"
            )));
        }
        return Ok(model);
    }

    if let Some(model) = model_override {
        if let Some(provider) = args.provider.as_deref()
            && model.provider != provider
        {
            if let Some(model) = first_model_for_provider(provider) {
                return Ok(model);
            }
            return Err(ApplicationError::UnknownModel(provider.to_string()));
        }
        return Ok(model);
    }

    if let Some(model_id) = default_model {
        let model = ai::api::model::lookup_model(model_id)
            .ok_or_else(|| ApplicationError::UnknownModel(model_id.to_string()))?;
        if let Some(provider) = effective_provider
            && model.provider != provider
        {
            if let Some(model) = first_model_for_provider(provider) {
                return Ok(model);
            }
            return Err(ApplicationError::UnknownModel(provider.to_string()));
        }
        return Ok(model);
    }

    if let Some(provider) = effective_provider {
        if let Some(model) = first_model_for_provider(provider) {
            return Ok(model);
        }
        return Err(ApplicationError::UnknownModel(provider.to_string()));
    }

    ai::api::model::lookup_model(DEFAULT_MODEL_ID)
        .ok_or_else(|| ApplicationError::UnknownModel(DEFAULT_MODEL_ID.to_string()))
}

fn first_model_for_provider(provider: &str) -> Option<Model> {
    let mut models = ai::api::model::all_models()
        .iter()
        .filter(|model| model.provider == provider)
        .cloned()
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.into_iter().next()
}

#[allow(
    clippy::too_many_arguments,
    reason = "public bootstrap compatibility keeps established configuration inputs explicit"
)]
#[cfg(test)]
pub fn build_agent_config(
    model: Model,
    system_prompt: Option<String>,
    max_turns: Option<u32>,
    api_key: Option<String>,
    thinking_level: Option<ThinkingLevel>,
    tool_execution: Option<ToolExecutionMode>,
    resources: AgentResources,
    settings: Option<&crate::config::Settings>,
) -> AgentConfig {
    build_agent_config_with_auth_diagnostics(
        model,
        system_prompt,
        max_turns,
        api_key,
        Vec::new(),
        thinking_level,
        tool_execution,
        resources,
        settings,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "bootstrap assembly keeps resolved authentication and runtime inputs explicit"
)]
pub(crate) fn build_agent_config_with_auth_diagnostics(
    model: Model,
    system_prompt: Option<String>,
    max_turns: Option<u32>,
    api_key: Option<String>,
    auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    thinking_level: Option<ThinkingLevel>,
    tool_execution: Option<ToolExecutionMode>,
    resources: AgentResources,
    settings: Option<&crate::config::Settings>,
) -> AgentConfig {
    let mut stream_options = api_key.map(|api_key| StreamOptions {
        api_key: Some(api_key),
        ..Default::default()
    });
    if !auth_diagnostics.is_empty() {
        stream_options
            .get_or_insert_with(StreamOptions::default)
            .auth_diagnostics
            .extend(auth_diagnostics);
    }
    if let Some(settings) = settings
        && settings.retry.enabled
    {
        let opts = stream_options.get_or_insert_with(StreamOptions::default);
        opts.max_retries = Some(settings.retry.max_retries);
        opts.max_retry_delay_ms = Some(settings.retry.base_delay_ms);
    }
    if let Some(settings) = settings
        && settings.http_idle_timeout_ms > 0
    {
        let opts = stream_options.get_or_insert_with(StreamOptions::default);
        opts.timeout_ms = Some(settings.http_idle_timeout_ms);
    }
    let mut config = AgentConfig::new(model);
    config.system_prompt = Some(system_prompt.unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()));
    config.max_turns = max_turns;
    config.stream_options = stream_options;
    if let Some(settings) = settings
        && settings.compaction.enabled
    {
        config.compaction = Some(CompactionConfig {
            settings: CompactionSettings {
                enabled: true,
                reserve_tokens: settings.compaction.reserve_tokens,
                keep_recent_tokens: settings.compaction.keep_recent_tokens,
            },
            custom_instructions: None,
        });
    }
    if let Some(settings) = settings {
        config.steering_mode = settings
            .steering_mode
            .parse()
            .unwrap_or(agent_core::api::agent::QueueMode::OneAtATime);
        config.follow_up_mode = settings
            .follow_up_mode
            .parse()
            .unwrap_or(agent_core::api::agent::QueueMode::OneAtATime);
    }
    let settings_thinking_level = settings
        .and_then(|settings| settings.default_thinking_level.as_deref())
        .and_then(|level| level.parse::<ThinkingLevel>().ok());
    if let Some(tl) = thinking_level.or(settings_thinking_level) {
        config.thinking_level = tl;
    }
    if let Some(te) = tool_execution {
        config.tool_execution = te;
    }
    config.resources = resources;
    config
}

pub fn effective_session_dir(
    args: &CodingAgentInvocationOptions,
    settings: &crate::config::Settings,
) -> Option<PathBuf> {
    args.session_dir
        .as_deref()
        .or(settings.session_dir.as_deref())
        .map(PathBuf::from)
}

pub fn effective_no_context_files(
    args: &CodingAgentInvocationOptions,
    settings: &crate::config::Settings,
) -> bool {
    args.no_context_files || settings.no_context_files
}

#[derive(Clone, Debug)]
pub enum PromptInvocation {
    Text(String),
    Content(Vec<ai::api::conversation::ContentBlock>),
    Compact {
        custom_instructions: Option<String>,
    },
    Skill {
        name: String,
        additional_instructions: Option<String>,
    },
    PromptTemplate {
        name: String,
        args: Vec<String>,
    },
}
