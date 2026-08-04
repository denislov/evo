use crate::error::CliError;
use coding_agent::api::authorization::ToolAuthorizationMode;
use coding_agent::api::embedding::{
    CodingAgentInvocationOptions, CodingAgentSessionSelection, CodingAgentThinkingLevel,
    CodingAgentToolExecutionMode,
};
use coding_agent::api::operation::PromptTurnMode;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliMode {
    Print,
    Json,
    Rpc,
}

impl FromStr for CliMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "print" => Ok(Self::Print),
            "json" => Ok(Self::Json),
            "rpc" => Ok(Self::Rpc),
            other => Err(format!("unknown mode: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub print: bool,
    pub mode: CliMode,
    pub mode_explicit: bool,
    pub prompt: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub models: Option<String>,
    pub list_models: Option<Option<String>>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
    /// Optional turn ceiling for the agent loop. `None` (the default) keeps
    /// the loop running until the model finishes naturally, matching the TS
    /// `pi/packages/agent` behavior. Use `--max-turns N` to cap.
    pub max_turns: Option<u32>,
    pub help: bool,
    pub version: bool,
    pub continue_session: bool,
    pub resume: bool,
    pub no_session: bool,
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub fork: Option<String>,
    pub session_dir: Option<String>,
    pub name: Option<String>,
    pub thinking: Option<CodingAgentThinkingLevel>,
    pub permission_mode: Option<ToolAuthorizationMode>,
    pub tool_execution: Option<CodingAgentToolExecutionMode>,
    pub skills: Vec<String>,
    pub prompt_templates: Vec<String>,
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
    pub verbose: bool,
    pub json: bool,
    pub offline: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            print: false,
            mode: CliMode::Print,
            mode_explicit: false,
            prompt: None,
            provider: None,
            model: None,
            models: None,
            list_models: None,
            api_key: None,
            system_prompt: None,
            append_system_prompt: Vec::new(),
            max_turns: None,
            help: false,
            version: false,
            continue_session: false,
            resume: false,
            no_session: false,
            session: None,
            session_id: None,
            fork: None,
            session_dir: None,
            name: None,
            thinking: None,
            permission_mode: None,
            tool_execution: None,
            skills: Vec::new(),
            prompt_templates: Vec::new(),
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
            verbose: false,
            json: false,
            offline: false,
        }
    }
}

impl CliArgs {
    pub(crate) fn invocation_options(&self) -> CodingAgentInvocationOptions {
        CodingAgentInvocationOptions {
            prompt_mode: match self.mode {
                CliMode::Print => PromptTurnMode::Print,
                CliMode::Json => PromptTurnMode::Json,
                CliMode::Rpc => PromptTurnMode::Rpc,
            },
            prompt: self.prompt.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            model_rotation: self.models.clone(),
            api_key: self.api_key.clone(),
            system_prompt: self.system_prompt.clone(),
            append_system_prompt: self.append_system_prompt.clone(),
            max_turns: self.max_turns,
            session: if self.no_session {
                CodingAgentSessionSelection::Disabled
            } else if let Some(target) = &self.fork {
                CodingAgentSessionSelection::ForkTarget(target.clone())
            } else if let Some(target) = &self.session {
                CodingAgentSessionSelection::OpenTarget(target.clone())
            } else if let Some(session_id) = &self.session_id {
                CodingAgentSessionSelection::OpenOrCreateId(session_id.clone())
            } else if self.continue_session || self.resume {
                CodingAgentSessionSelection::ContinueMostRecent
            } else {
                CodingAgentSessionSelection::Default
            },
            session_dir: self.session_dir.clone(),
            session_name: self.name.clone(),
            thinking: self.thinking,
            permission_mode: self.permission_mode,
            tool_execution: self.tool_execution,
            skill_paths: self.skills.clone(),
            prompt_template_paths: self.prompt_templates.clone(),
            skill: self.skill.clone(),
            prompt_template: self.prompt_template.clone(),
            template_args: self.template_args.clone(),
            no_context_files: self.no_context_files,
            no_skills: self.no_skills,
            no_prompt_templates: self.no_prompt_templates,
            no_themes: self.no_themes,
            theme_paths: self.theme_paths.clone(),
            tools: self.tools.clone(),
            exclude_tools: self.exclude_tools.clone(),
            no_tools: self.no_tools,
            no_builtin_tools: self.no_builtin_tools,
        }
    }
}

pub fn help_text() -> String {
    let help = format!(
        "coding-agent {}\n\nUsage:\n  coding-agent -p <prompt>\n\nOptions:\n  -p, --print              Run one prompt and print the assistant response\n  --mode <mode>            Headless mode: print|json|rpc\n  --provider <id>          Provider preference for model selection\n  --model <id>             Model id from the built-in Rust model table\n  --models <list>          Comma-separated model rotation globs, optionally model:thinking\n  --list-models [search]   List models, optionally fuzzy-filtered by search text\n  --json                   Emit JSON for --list-models\n  --api-key <key>          API key passed to the selected provider\n  --system-prompt <text>   System prompt override\n  --append-system-prompt <text> Append to system prompt (repeatable)\n  --max-turns <n>          Optional cap on agent loop turns (default: unlimited, matches TS pi)\n  --thinking <level>       Thinking level: off|minimal|low|medium|high|xhigh\n                           DeepSeek effective effort: minimal/low/medium/high=high, xhigh=max\n  --permission-mode <mode> Permission mode: plan (read-only) | ask (prompt first) | yolo (auto)\n  --tool-execution <mode>  Tool execution mode: parallel|sequential\n  --tools, -t <names>      Comma-separated builtin tool allowlist\n  --exclude-tools, -xt <names> Comma-separated builtin tool denylist\n  --no-tools               Disable all tools\n  --no-builtin-tools       Do not register builtin tools\n  --skills <dir>           Directory to load skills from (repeatable)\n  --prompt-templates <p>   Path to load prompt templates from (repeatable)\n  --no-context-files       Disable AGENTS.md / CLAUDE.md discovery\n  --no-skills              Disable skill discovery\n  --no-prompt-templates    Disable prompt template discovery\n  --no-themes              Disable theme discovery\n  --theme <path>            Load a theme file or directory (repeatable)\n  --skill <name>           Invoke a loaded skill by name\n  --prompt-template <name> Invoke a prompt template by name\n  --template-arg <value>   Argument for prompt template (repeatable)\n  --verbose                Emit verbose diagnostics\n  --offline                Avoid network-dependent behavior where supported\n  -h, --help               Show help\n  -v, --version            Show version\n\nSession Options:\n  -c, --continue           Continue the most recent session\n  -r, --resume             Resume the most recent session\n  --no-session             Disable session persistence\n  --session <path|id>      Open a specific session by path or id prefix\n  --session-id <id>        Open or create a session by exact id\n  --fork <path|id>         Fork an existing session\n  --session-dir <dir>      Directory to store session files\n  --name <name>            Name for the current session\n  -n <name>                Short form of --name\n",
        env!("CARGO_PKG_VERSION")
    );
    help
}

fn take_value(raw: &[String], index: &mut usize, flag: &str) -> Result<String, CliError> {
    let next_index = *index + 1;
    let value = raw
        .get(next_index)
        .ok_or_else(|| CliError::MissingValue(flag.to_string()))?;
    *index = next_index;
    Ok(value.clone())
}

fn parse_max_turns(value: String) -> Result<u32, CliError> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| CliError::InvalidMaxTurns(value.clone()))?;
    if parsed == 0 {
        return Err(CliError::InvalidMaxTurns(value));
    }
    Ok(parsed)
}

fn has_session_target(args: &CliArgs) -> bool {
    args.continue_session
        || args.resume
        || args.session.is_some()
        || args.session_id.is_some()
        || args.fork.is_some()
}

pub fn parse_args<I>(args: I) -> Result<CliArgs, CliError>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = CliArgs::default();
    let mut prompt_parts = Vec::new();
    let raw: Vec<String> = args.into_iter().collect();
    let mut i = 0;

    while i < raw.len() {
        let arg = &raw[i];
        match arg.as_str() {
            "-p" | "--print" => {
                parsed.print = true;
                if let Some(next) = raw.get(i + 1)
                    && (!next.starts_with('-') || next.starts_with("---"))
                {
                    prompt_parts.push(next.clone());
                    i += 1;
                }
            }
            "-h" | "--help" => parsed.help = true,
            "-v" | "--version" => parsed.version = true,
            "--mode" => {
                let value = take_value(&raw, &mut i, "--mode")?;
                parsed.mode = value.parse().map_err(CliError::InvalidInput)?;
                parsed.mode_explicit = true;
            }
            "--provider" => parsed.provider = Some(take_value(&raw, &mut i, "--provider")?),
            "--model" => parsed.model = Some(take_value(&raw, &mut i, "--model")?),
            "--models" => parsed.models = Some(take_value(&raw, &mut i, "--models")?),
            "--list-models" => {
                let search = raw.get(i + 1).and_then(|next| {
                    if next.starts_with('-') || next.starts_with('@') {
                        None
                    } else {
                        Some(next.clone())
                    }
                });
                if search.is_some() {
                    i += 1;
                }
                parsed.list_models = Some(search);
            }
            "--json" => parsed.json = true,
            "--api-key" => parsed.api_key = Some(take_value(&raw, &mut i, "--api-key")?),
            "--system-prompt" => {
                parsed.system_prompt = Some(take_value(&raw, &mut i, "--system-prompt")?)
            }
            "--append-system-prompt" => {
                let val = take_value(&raw, &mut i, "--append-system-prompt")?;
                parsed.append_system_prompt.push(val);
            }
            "--max-turns" => {
                let value = take_value(&raw, &mut i, "--max-turns")?;
                parsed.max_turns = Some(parse_max_turns(value)?);
            }
            "-c" | "--continue" => parsed.continue_session = true,
            "-r" | "--resume" => parsed.resume = true,
            "--no-session" => parsed.no_session = true,
            "--session" => parsed.session = Some(take_value(&raw, &mut i, "--session")?),
            "--session-id" => parsed.session_id = Some(take_value(&raw, &mut i, "--session-id")?),
            "--fork" => parsed.fork = Some(take_value(&raw, &mut i, "--fork")?),
            "--session-dir" => {
                parsed.session_dir = Some(take_value(&raw, &mut i, "--session-dir")?)
            }
            "--name" => parsed.name = Some(take_value(&raw, &mut i, "--name")?),
            "-n" => parsed.name = Some(take_value(&raw, &mut i, "-n")?),
            "--thinking" => {
                let val = take_value(&raw, &mut i, "--thinking")?;
                parsed.thinking = Some(val.parse().map_err(CliError::InvalidInput)?);
            }
            "--tool-execution" => {
                let val = take_value(&raw, &mut i, "--tool-execution")?;
                parsed.tool_execution = Some(val.parse().map_err(CliError::InvalidInput)?);
            }
            "--permission-mode" => {
                let val = take_value(&raw, &mut i, "--permission-mode")?;
                parsed.permission_mode = Some(val.parse().map_err(CliError::InvalidInput)?);
            }
            "--skills" => {
                let val = take_value(&raw, &mut i, "--skills")?;
                parsed.skills.push(val);
            }
            "--prompt-templates" => {
                let val = take_value(&raw, &mut i, "--prompt-templates")?;
                parsed.prompt_templates.push(val);
            }
            "--no-context-files" => parsed.no_context_files = true,
            "--no-skills" => parsed.no_skills = true,
            "--no-prompt-templates" => parsed.no_prompt_templates = true,
            "--no-themes" => parsed.no_themes = true,
            "--theme" => {
                let val = take_value(&raw, &mut i, "--theme")?;
                parsed.theme_paths.push(val);
            }
            "--skill" => parsed.skill = Some(take_value(&raw, &mut i, "--skill")?),
            "--prompt-template" => {
                parsed.prompt_template = Some(take_value(&raw, &mut i, "--prompt-template")?)
            }
            "--template-arg" => {
                let val = take_value(&raw, &mut i, "--template-arg")?;
                parsed.template_args.push(val);
            }
            "--tools" | "-t" => {
                let val = take_value(&raw, &mut i, arg)?;
                parsed.tools.extend(split_csv(&val));
            }
            "--exclude-tools" | "-xt" => {
                let val = take_value(&raw, &mut i, arg)?;
                parsed.exclude_tools.extend(split_csv(&val));
            }
            "--no-tools" => parsed.no_tools = true,
            "--no-builtin-tools" => parsed.no_builtin_tools = true,
            "--verbose" => parsed.verbose = true,
            "--offline" => parsed.offline = true,
            value if value.starts_with("--") => {
                return Err(CliError::UnknownFlag(value.to_string()));
            }
            value if value.starts_with('-') => {
                return Err(CliError::UnknownFlag(value.to_string()));
            }
            value => prompt_parts.push(value.to_string()),
        }
        i += 1;
    }

    if !prompt_parts.is_empty() {
        parsed.prompt = Some(prompt_parts.join(" "));
    }

    if parsed.print {
        if parsed.mode_explicit && parsed.mode != CliMode::Print {
            return Err(CliError::InvalidInput(
                "--print can only be combined with --mode print".into(),
            ));
        }
        parsed.mode = CliMode::Print;
    }

    if parsed.mode == CliMode::Rpc && parsed.prompt.is_some() {
        return Err(CliError::InvalidInput(
            "unsupported mode input: rpc does not accept positional prompt".into(),
        ));
    }

    let session_target_count = parsed.continue_session as u32
        + parsed.resume as u32
        + parsed.session.is_some() as u32
        + parsed.session_id.is_some() as u32
        + parsed.fork.is_some() as u32;
    if session_target_count > 1 {
        return Err(CliError::InvalidSessionFlags(
            "multiple session target flags are not allowed".into(),
        ));
    }

    if parsed.no_session && has_session_target(&parsed) {
        return Err(CliError::InvalidSessionFlags(
            "--no-session cannot be combined with session selection flags".into(),
        ));
    }

    if parsed.no_session && parsed.name.is_some() {
        return Err(CliError::InvalidSessionFlags(
            "--no-session cannot be combined with session selection flags".into(),
        ));
    }

    if parsed.skill.is_some() && parsed.prompt_template.is_some() {
        return Err(CliError::InvalidInput(
            "--skill and --prompt-template cannot be used together".into(),
        ));
    }

    if parsed.no_tools && (!parsed.tools.is_empty() || !parsed.exclude_tools.is_empty()) {
        return Err(CliError::InvalidInput(
            "--no-tools cannot be combined with --tools or --exclude-tools".into(),
        ));
    }

    if parsed.json && parsed.list_models.is_none() {
        return Err(CliError::InvalidInput(
            "--json can only be used with --list-models".into(),
        ));
    }

    Ok(parsed)
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}
