use crate::app::bootstrap::{
    ApplicationRunOptions, PromptInvocation, SessionRunOptions, effective_no_context_files,
    effective_session_dir, select_model,
};
use crate::app::error::ApplicationError;
use crate::app::invocation::{CodingAgentInvocationOptions, CodingAgentSessionSelection};
use crate::app::prompt_input::{self as prompt_input, ProcessedPromptInput};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::app::session::ResolvedSessionTarget;
use crate::config::{self, Config, ConfigPaths};
use crate::profiles::{ProfileRegistry, ProfileRegistryOptions};
use crate::resources::{self, LoadedResources};
use crate::tools::{self, ToolFilter};
use agent_core::api::resources::{
    AgentResources, DiagnosticSeverity as ResourceDiagnosticSeverity, ResourceDiagnostic,
};
use ai::api::auth::ProviderAuthDiagnostic;
use ai::api::client::AiClient;
use ai::api::model::Model;
use ai::api::transport::TransportConfig;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationDiagnostic {
    pub severity: ApplicationDiagnosticSeverity,
    pub message: String,
    pub source: Option<PathBuf>,
    pub code: Option<String>,
}

pub struct ResolvedApplicationContext {
    pub cwd: PathBuf,
    pub invocation_options: CodingAgentInvocationOptions,
    pub config: Config,
    pub config_paths: ConfigPaths,
    pub model: Model,
    pub api_key: Option<String>,
    pub auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    pub loaded_resources: LoadedResources,
    pub context_files: Vec<crate::resources::ContextFile>,
    pub system_prompt: Option<String>,
    pub tools: Vec<agent_core::api::tool::AgentTool>,
    pub register_builtins: bool,
    pub global_config_only: bool,
    pub ai_client: Option<AiClient>,
    pub session: Option<SessionRunOptions>,
    pub session_target: Option<ResolvedSessionTarget>,
    pub session_name: Option<String>,
    pub agent_resources: AgentResources,
    pub diagnostics: Vec<ApplicationDiagnostic>,
}

pub(crate) struct ResolvedPromptRequest {
    pub context: ResolvedApplicationContext,
    pub invocation: PromptInvocation,
    pub session_options: PromptRuntimeOptions,
}

pub fn resolve_application_context(
    parsed: CodingAgentInvocationOptions,
    options: ApplicationRunOptions,
    cwd: PathBuf,
    global_dir: PathBuf,
) -> Result<ResolvedApplicationContext, ApplicationError> {
    let config_paths = config::ConfigPaths {
        global_dir,
        project_dir: cwd.join(".evo"),
    };
    let mut config_diags = Vec::new();
    let global_config_only = options.global_config_only;
    let config = Config {
        settings: if global_config_only {
            config::settings::load_global_settings(&config_paths, &mut config_diags)
        } else {
            config::settings::load_settings(&config_paths, &mut config_diags)
        },
        auth: config::auth::AuthStore::load(&config_paths.global_auth(), &mut config_diags),
    };
    let mut diagnostics = config_diags
        .iter()
        .map(ApplicationDiagnostic::from_config)
        .collect::<Vec<_>>();

    let model = select_model(
        &parsed,
        config.settings.default_provider.as_deref(),
        config.settings.default_model.as_deref(),
        options.model_override,
    )?;

    let provider = model.provider.clone();
    let (api_key, auth_diagnostics) = resolve_api_key(
        &provider,
        parsed.api_key.as_deref(),
        &config,
        &mut diagnostics,
    );
    let loaded_resources = resources::load_application_resources_with_options(
        &parsed.skill_paths,
        &parsed.prompt_template_paths,
        &cwd,
        &config_paths.global_dir,
        resources::ResourceLoadOptions {
            no_skills: parsed.no_skills,
            no_prompt_templates: parsed.no_prompt_templates,
            no_themes: parsed.no_themes,
            skill_paths: config.settings.skills.clone(),
            prompt_paths: config.settings.prompts.clone(),
            theme_paths: {
                let mut paths = config.settings.themes.clone();
                paths.extend(parsed.theme_paths.iter().cloned());
                paths
            },
            theme: config.settings.theme.clone(),
            include_project_resources: !global_config_only,
        },
    )?;
    diagnostics.extend(
        loaded_resources
            .diagnostics
            .iter()
            .map(ApplicationDiagnostic::from_resource),
    );

    validate_selected_resources(&parsed, &loaded_resources)?;

    let context_files = resources::discover_context_files_with_project(
        &cwd,
        &config_paths.global_dir,
        effective_no_context_files(&parsed, &config.settings),
        !global_config_only,
    );
    let system_prompt = resolve_system_prompt(&parsed, &cwd, &context_files);
    let tools = tools::filter_tools(
        options.tools,
        &ToolFilter {
            allow: parsed.tools.clone(),
            deny: parsed.exclude_tools.clone(),
            no_tools: parsed.no_tools,
            no_builtin_tools: parsed.no_builtin_tools,
        },
    );

    let session = resolve_session_options(&parsed, &config, options.session);
    let session_target = resolve_session_target(&parsed);
    let session_name = parsed.session_name.clone();
    let agent_resources = resources::build_agent_resources(
        loaded_resources.skills.clone(),
        loaded_resources.prompt_templates.clone(),
    );
    let ai_client = match options.ai_client {
        Some(client) => Some(client),
        None => Some(
            AiClient::try_with_auth_resolver_and_transport(
                Arc::new(config::auth::AuthStoreProviderAuthResolver::new(
                    provider,
                    api_key.clone(),
                    auth_diagnostics.clone(),
                    config.auth.clone(),
                )),
                TransportConfig::new(
                    config.settings.http_proxy.clone(),
                    config.settings.websocket_connect_timeout_ms,
                ),
            )
            .map_err(ApplicationError::InvalidInput)?,
        ),
    };

    Ok(ResolvedApplicationContext {
        cwd,
        invocation_options: parsed,
        config,
        config_paths,
        model,
        api_key,
        auth_diagnostics,
        loaded_resources,
        context_files,
        system_prompt,
        tools,
        register_builtins: options.register_builtins,
        global_config_only,
        ai_client,
        session,
        session_target,
        session_name,
        agent_resources,
        diagnostics,
    })
}

pub fn resolve_application_context_from_options(
    parsed: CodingAgentInvocationOptions,
    options: ApplicationRunOptions,
) -> Result<ResolvedApplicationContext, ApplicationError> {
    let cwd = options.session.cwd.clone();
    let global_dir = config::resolve_paths(&cwd).global_dir;
    resolve_application_context(parsed, options, cwd, global_dir)
}

pub fn resolve_profile_registry(
    context: &ResolvedApplicationContext,
) -> Result<ProfileRegistry, ApplicationError> {
    let options =
        ProfileRegistryOptions::new().with_user_root(context.config_paths.global_dir.clone());
    let options = if context.global_config_only {
        options
    } else {
        options.with_project_root(context.config_paths.project_dir.clone())
    };
    Ok(ProfileRegistry::load(options)?)
}

pub fn resolve_provider_api_key(
    provider: &str,
    invocation_api_key: Option<&str>,
    auth: &crate::config::AuthStore,
) -> (
    Option<String>,
    Vec<ProviderAuthDiagnostic>,
    Vec<ApplicationDiagnostic>,
) {
    let mut key_diags = Vec::new();
    let resolved =
        config::auth::resolve_api_key(provider, invocation_api_key, auth, &mut key_diags);
    let auth_diagnostics = resolved
        .as_ref()
        .map(|resolved| resolved.provider_auth_diagnostic())
        .into_iter()
        .collect();
    let diagnostics = key_diags
        .iter()
        .map(ApplicationDiagnostic::from_config)
        .collect();
    (
        resolved.map(|resolved| resolved.value),
        auth_diagnostics,
        diagnostics,
    )
}

pub fn configured_model_choices(
    current_model: &Model,
    invocation_api_key: Option<&str>,
    auth: &crate::config::AuthStore,
) -> Vec<Model> {
    let mut configured_providers = BTreeSet::new();
    for provider in ai::api::model::get_providers() {
        if provider_has_configured_key(&provider, &current_model.provider, invocation_api_key, auth)
        {
            configured_providers.insert(provider);
        }
    }

    let mut models = ai::api::model::all_models()
        .iter()
        .filter(|model| configured_providers.contains(&model.provider))
        .cloned()
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.id.cmp(&right.id))
    });
    if let Some(current_index) = models
        .iter()
        .position(|model| model.provider == current_model.provider && model.id == current_model.id)
    {
        let current = models.remove(current_index);
        models.insert(0, current);
    }
    models
}

pub fn rotation_model_choices(
    models_arg: Option<&str>,
    provider: Option<&str>,
    enabled_models: Option<&[String]>,
) -> Result<Vec<Model>, ApplicationError> {
    let models_arg = match models_arg {
        Some(arg) => Some(arg.to_string()),
        None => enabled_models
            .filter(|list| !list.is_empty())
            .map(|list| list.join(",")),
    };
    let Some(models_arg) = models_arg else {
        return Ok(Vec::new());
    };
    let rotation = crate::app::model_selection::parse_model_rotation(&models_arg)?;
    let mut candidates = ai::api::model::all_models().to_vec();
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    if let Some(provider) = provider {
        candidates.retain(|model| model.provider == provider);
    }
    Ok(candidates
        .into_iter()
        .filter(|model| rotation.matches(&model.id) || rotation.matches(&model.name))
        .collect())
}

fn provider_has_configured_key(
    provider: &str,
    current_provider: &str,
    invocation_api_key: Option<&str>,
    auth: &crate::config::AuthStore,
) -> bool {
    if provider == current_provider && invocation_api_key.is_some_and(|key| !key.is_empty()) {
        return true;
    }
    let mut diagnostics = Vec::new();
    config::auth::resolve_api_key(provider, None, auth, &mut diagnostics).is_some()
}

pub fn resolve_prompt_request(
    parsed: CodingAgentInvocationOptions,
    options: ApplicationRunOptions,
    stdin: Option<String>,
    cwd: PathBuf,
    global_dir: PathBuf,
) -> Result<ResolvedPromptRequest, ApplicationError> {
    let prompt = match parsed.prompt.clone() {
        Some(prompt) if !prompt.trim().is_empty() => prompt,
        _ if stdin
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()) =>
        {
            String::new()
        }
        _ => return Err(ApplicationError::MissingPrompt),
    };
    let merged_prompt = prompt_input::merge_stdin_prompt(&prompt, stdin.as_deref());
    let context = resolve_application_context(parsed, options, cwd, global_dir)?;
    let processed_prompt = prompt_input::process_at_file_references_with_processing_options(
        &merged_prompt,
        &context.cwd,
        prompt_input::ImageProcessingOptions::from_settings(&context.config.settings),
    )?;
    let invocation = resolve_invocation(&context, &processed_prompt);

    let session_options = PromptRuntimeOptions {
        model: context.model.clone(),
        api_key: context.api_key.clone(),
        auth_diagnostics: context.auth_diagnostics.clone(),
        system_prompt: context.system_prompt.clone(),
        max_turns: context.invocation_options.max_turns,
        tools: context.tools.clone(),
        register_builtins: context.register_builtins,
        ai_client: context.ai_client.clone(),
        session: context.session.clone(),
        session_target: context.session_target.clone(),
        session_name: context.session_name.clone(),
        thinking_level: context.invocation_options.thinking.map(Into::into),
        tool_execution: context.invocation_options.tool_execution.map(Into::into),
        resources: context.agent_resources.clone(),
        settings: Some(context.config.settings.clone()),
        invocation: invocation.clone(),
    };

    Ok(ResolvedPromptRequest {
        context,
        invocation,
        session_options,
    })
}

pub fn resolve_session_target(
    parsed: &CodingAgentInvocationOptions,
) -> Option<ResolvedSessionTarget> {
    match &parsed.session {
        CodingAgentSessionSelection::Default | CodingAgentSessionSelection::Disabled => None,
        CodingAgentSessionSelection::ContinueMostRecent => {
            Some(ResolvedSessionTarget::ContinueMostRecent)
        }
        CodingAgentSessionSelection::OpenTarget(target) => {
            Some(ResolvedSessionTarget::OpenTarget(target.clone()))
        }
        CodingAgentSessionSelection::OpenOrCreateId(session_id) => {
            Some(ResolvedSessionTarget::OpenOrCreateId(session_id.clone()))
        }
        CodingAgentSessionSelection::ForkTarget(target) => {
            Some(ResolvedSessionTarget::ForkTarget(target.clone()))
        }
    }
}

fn resolve_api_key(
    provider: &str,
    invocation_api_key: Option<&str>,
    config: &Config,
    diagnostics: &mut Vec<ApplicationDiagnostic>,
) -> (Option<String>, Vec<ProviderAuthDiagnostic>) {
    let mut key_diags = Vec::new();
    let resolved =
        config::auth::resolve_api_key(provider, invocation_api_key, &config.auth, &mut key_diags);
    diagnostics.extend(key_diags.iter().map(ApplicationDiagnostic::from_config));
    let auth_diagnostics = resolved
        .as_ref()
        .map(|resolved| resolved.provider_auth_diagnostic())
        .into_iter()
        .collect();
    (resolved.map(|resolved| resolved.value), auth_diagnostics)
}

impl ApplicationDiagnostic {
    pub fn from_config(diagnostic: &config::ConfigDiagnostic) -> Self {
        let severity = match diagnostic.severity {
            config::DiagnosticSeverity::Warn => ApplicationDiagnosticSeverity::Warning,
        };
        Self {
            severity,
            message: diagnostic.message.clone(),
            source: diagnostic.source.clone(),
            code: Some("config".to_string()),
        }
    }

    pub fn from_resource(diagnostic: &ResourceDiagnostic) -> Self {
        let severity = match diagnostic.severity {
            ResourceDiagnosticSeverity::Info => ApplicationDiagnosticSeverity::Info,
            ResourceDiagnosticSeverity::Warning => ApplicationDiagnosticSeverity::Warning,
            ResourceDiagnosticSeverity::Error => ApplicationDiagnosticSeverity::Error,
        };
        Self {
            severity,
            message: diagnostic.message.clone(),
            source: Some(diagnostic.path.clone()),
            code: Some(diagnostic.code.clone()),
        }
    }
}

pub fn format_application_diagnostics(diagnostics: &[ApplicationDiagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        let label = match diagnostic.severity {
            ApplicationDiagnosticSeverity::Info => "info",
            ApplicationDiagnosticSeverity::Warning => "warning",
            ApplicationDiagnosticSeverity::Error => "error",
        };
        match diagnostic.code.as_deref() {
            Some("config") => match &diagnostic.source {
                Some(path) => out.push_str(&format!(
                    "config {label}: {} ({})\n",
                    diagnostic.message,
                    path.display()
                )),
                None => out.push_str(&format!("config {label}: {}\n", diagnostic.message)),
            },
            Some(code) => match &diagnostic.source {
                Some(path) => out.push_str(&format!(
                    "resource {}: {} (code: {})\n",
                    path.display(),
                    diagnostic.message,
                    code
                )),
                None => out.push_str(&format!(
                    "resource {label}: {} (code: {})\n",
                    diagnostic.message, code
                )),
            },
            None => match &diagnostic.source {
                Some(path) => out.push_str(&format!(
                    "{label}: {} ({})\n",
                    diagnostic.message,
                    path.display()
                )),
                None => out.push_str(&format!("{label}: {}\n", diagnostic.message)),
            },
        }
    }
    out
}

fn validate_selected_resources(
    parsed: &CodingAgentInvocationOptions,
    loaded: &LoadedResources,
) -> Result<(), ApplicationError> {
    if let Some(ref skill_name) = parsed.skill
        && resources::find_skill(&loaded.skills, skill_name).is_none()
    {
        return Err(ApplicationError::InvalidInput(format!(
            "skill '{skill_name}' not found in loaded skills"
        )));
    }
    if let Some(ref template_name) = parsed.prompt_template
        && resources::find_template(&loaded.prompt_templates, template_name).is_none()
    {
        return Err(ApplicationError::InvalidInput(format!(
            "prompt template '{template_name}' not found in loaded templates"
        )));
    }
    Ok(())
}

fn resolve_system_prompt(
    parsed: &CodingAgentInvocationOptions,
    cwd: &std::path::Path,
    context_files: &[crate::resources::ContextFile],
) -> Option<String> {
    let has_custom = parsed.system_prompt.is_some();
    let mut system_prompt = parsed.system_prompt.clone();
    if !context_files.is_empty() || !parsed.append_system_prompt.is_empty() {
        let mut parts = Vec::new();
        if let Some(base) = system_prompt.take() {
            parts.push(base);
        }
        // Wrap context files in <project_context> / <project_instructions>,
        // mirroring TS `buildSystemPrompt` in `system-prompt.ts`.
        if !context_files.is_empty() {
            let mut ctx_block = String::from(
                "<project_context>\n\nProject-specific instructions and guidelines:\n\n",
            );
            for file in context_files {
                ctx_block.push_str(&format!(
                    "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
                    file.path.display(),
                    file.content
                ));
            }
            ctx_block.push_str("</project_context>");
            parts.push(ctx_block);
        }
        parts.extend(parsed.append_system_prompt.clone());
        system_prompt = Some(parts.join("\n\n"));
    }
    // Append cwd suffix, mirroring TS's date and working directory footer.
    if let Some(ref mut prompt) = system_prompt
        && has_custom
    {
        let display_cwd = cwd.display().to_string().replace('\\', "/");
        *prompt = format!("{prompt}\nCurrent working directory: {display_cwd}");
    }
    system_prompt
}

fn resolve_session_options(
    parsed: &CodingAgentInvocationOptions,
    config: &Config,
    mut session_options: SessionRunOptions,
) -> Option<SessionRunOptions> {
    if matches!(parsed.session, CodingAgentSessionSelection::Disabled) {
        return None;
    }
    if let Some(dir) = effective_session_dir(parsed, &config.settings) {
        session_options.session_dir = Some(dir);
    }
    Some(session_options)
}

fn resolve_invocation(
    context: &ResolvedApplicationContext,
    processed_prompt: &ProcessedPromptInput,
) -> PromptInvocation {
    if let Some(ref skill_name) = context.invocation_options.skill {
        PromptInvocation::Skill {
            name: skill_name.clone(),
            additional_instructions: None,
        }
    } else if let Some(ref template_name) = context.invocation_options.prompt_template {
        PromptInvocation::PromptTemplate {
            name: template_name.clone(),
            args: context.invocation_options.template_args.clone(),
        }
    } else if processed_prompt.images.is_empty() {
        PromptInvocation::Text(processed_prompt.text.clone())
    } else {
        PromptInvocation::Content(processed_prompt.content.clone())
    }
}
