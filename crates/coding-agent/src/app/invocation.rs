use std::fmt;
use std::str::FromStr;

use tool_contract::api::definition::ToolExecutionMode;

use crate::app::embedding::CodingAgentThinkingLevel;
use crate::runtime::facade::PromptTurnMode;

/// Product-semantic session selection requested by an application adapter.
#[derive(Default, Clone, PartialEq, Eq)]
pub enum CodingAgentSessionSelection {
    #[default]
    Default,
    Disabled,
    ContinueMostRecent,
    OpenTarget(String),
    OpenOrCreateId(String),
    ForkTarget(String),
}

impl fmt::Debug for CodingAgentSessionSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("Default"),
            Self::Disabled => formatter.write_str("Disabled"),
            Self::ContinueMostRecent => formatter.write_str("ContinueMostRecent"),
            Self::OpenTarget(_) => formatter.write_str("OpenTarget(..)"),
            Self::OpenOrCreateId(_) => formatter.write_str("OpenOrCreateId(..)"),
            Self::ForkTarget(_) => formatter.write_str("ForkTarget(..)"),
        }
    }
}

/// Product-owned tool scheduling policy selected by an application adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentToolExecutionMode {
    Parallel,
    Sequential,
}

impl FromStr for CodingAgentToolExecutionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "parallel" => Ok(Self::Parallel),
            "sequential" => Ok(Self::Sequential),
            other => Err(format!("unknown tool execution mode: {other}")),
        }
    }
}

impl From<CodingAgentToolExecutionMode> for ToolExecutionMode {
    fn from(value: CodingAgentToolExecutionMode) -> Self {
        match value {
            CodingAgentToolExecutionMode::Parallel => Self::Parallel,
            CodingAgentToolExecutionMode::Sequential => Self::Sequential,
        }
    }
}

impl From<ToolExecutionMode> for CodingAgentToolExecutionMode {
    fn from(value: ToolExecutionMode) -> Self {
        match value {
            ToolExecutionMode::Parallel => Self::Parallel,
            ToolExecutionMode::Sequential => Self::Sequential,
        }
    }
}

/// Product-semantic invocation inputs resolved by `coding-agent`.
///
/// This is input intent, not runtime authority. Provider models, credentials,
/// executable tools, loaded resources, complete configuration, repositories,
/// and durable paths are resolved and retained inside opaque product handles.
#[derive(Clone)]
pub struct CodingAgentInvocationOptions {
    pub prompt_mode: PromptTurnMode,
    pub prompt: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_rotation: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
    pub max_turns: Option<u32>,
    pub session: CodingAgentSessionSelection,
    pub session_dir: Option<String>,
    pub session_name: Option<String>,
    pub thinking: Option<CodingAgentThinkingLevel>,
    pub permission_mode: Option<crate::authorization::ToolAuthorizationMode>,
    pub tool_execution: Option<CodingAgentToolExecutionMode>,
    pub skill_paths: Vec<String>,
    pub prompt_template_paths: Vec<String>,
    pub skill: Option<String>,
    pub prompt_template: Option<String>,
    pub template_args: Vec<String>,
    pub no_context_files: bool,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
    pub theme_paths: Vec<String>,
    pub tools: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub no_tools: bool,
    pub no_builtin_tools: bool,
}

impl Default for CodingAgentInvocationOptions {
    fn default() -> Self {
        Self {
            prompt_mode: PromptTurnMode::Print,
            prompt: None,
            provider: None,
            model: None,
            model_rotation: None,
            api_key: None,
            system_prompt: None,
            append_system_prompt: Vec::new(),
            max_turns: None,
            session: CodingAgentSessionSelection::Default,
            session_dir: None,
            session_name: None,
            thinking: None,
            permission_mode: None,
            tool_execution: None,
            skill_paths: Vec::new(),
            prompt_template_paths: Vec::new(),
            skill: None,
            prompt_template: None,
            template_args: Vec::new(),
            no_context_files: false,
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
            theme_paths: Vec::new(),
            tools: Vec::new(),
            exclude_tools: Vec::new(),
            no_tools: false,
            no_builtin_tools: false,
        }
    }
}

impl fmt::Debug for CodingAgentInvocationOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentInvocationOptions")
            .field("prompt_mode", &self.prompt_mode)
            .field("has_prompt", &self.prompt.is_some())
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("has_model_rotation", &self.model_rotation.is_some())
            .field("has_api_key", &self.api_key.is_some())
            .field("has_system_prompt", &self.system_prompt.is_some())
            .field(
                "append_system_prompt_count",
                &self.append_system_prompt.len(),
            )
            .field("max_turns", &self.max_turns)
            .field("session", &self.session)
            .field("has_session_dir", &self.session_dir.is_some())
            .field("has_session_name", &self.session_name.is_some())
            .field("thinking", &self.thinking)
            .field("permission_mode", &self.permission_mode)
            .field("tool_execution", &self.tool_execution)
            .field("skill_path_count", &self.skill_paths.len())
            .field(
                "prompt_template_path_count",
                &self.prompt_template_paths.len(),
            )
            .field("has_skill", &self.skill.is_some())
            .field("has_prompt_template", &self.prompt_template.is_some())
            .field("template_arg_count", &self.template_args.len())
            .field("no_context_files", &self.no_context_files)
            .field("no_skills", &self.no_skills)
            .field("no_prompt_templates", &self.no_prompt_templates)
            .field("no_themes", &self.no_themes)
            .field("theme_path_count", &self.theme_paths.len())
            .field("tool_allow_count", &self.tools.len())
            .field("tool_deny_count", &self.exclude_tools.len())
            .field("no_tools", &self.no_tools)
            .field("no_builtin_tools", &self.no_builtin_tools)
            .finish_non_exhaustive()
    }
}
