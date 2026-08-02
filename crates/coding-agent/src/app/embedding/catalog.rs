use super::*;

impl From<CodingAgentThinkingLevel> for ThinkingLevel {
    fn from(value: CodingAgentThinkingLevel) -> Self {
        match value {
            CodingAgentThinkingLevel::Off => Self::Off,
            CodingAgentThinkingLevel::Minimal => Self::Minimal,
            CodingAgentThinkingLevel::Low => Self::Low,
            CodingAgentThinkingLevel::Medium => Self::Medium,
            CodingAgentThinkingLevel::High => Self::High,
            CodingAgentThinkingLevel::XHigh => Self::XHigh,
        }
    }
}

impl From<ThinkingLevel> for CodingAgentThinkingLevel {
    fn from(value: ThinkingLevel) -> Self {
        match value {
            ThinkingLevel::Off => Self::Off,
            ThinkingLevel::Minimal => Self::Minimal,
            ThinkingLevel::Low => Self::Low,
            ThinkingLevel::Medium => Self::Medium,
            ThinkingLevel::High => Self::High,
            ThinkingLevel::XHigh => Self::XHigh,
        }
    }
}

pub(super) fn resolve_model(model_id: &str) -> Result<Model, CodingSessionError> {
    ai::api::model::lookup_model(model_id).ok_or_else(|| CodingSessionError::Config {
        message: format!("unknown model: {model_id}"),
    })
}

pub(super) fn validate_profile(
    registry: &ProfileRegistry,
    profile_id: &ProfileId,
) -> Result<(), CodingSessionError> {
    if registry.agent(profile_id.as_str()).is_some() {
        Ok(())
    } else {
        Err(CodingSessionError::Config {
            message: format!("unknown default agent profile: {profile_id}"),
        })
    }
}

pub(super) fn build_snapshot(
    options: &CodingAgentEmbeddingOptions,
    resolved: &ResolvedApplicationContext,
    profile_registry: &ProfileRegistry,
) -> CodingAgentEmbeddingSnapshot {
    let configured = configured_model_choices(&resolved.model, None, &resolved.config.auth)
        .into_iter()
        .map(|model| (model.provider, model.id))
        .collect::<BTreeSet<_>>();
    let mut models = model_catalog()
        .into_iter()
        .map(|model| {
            let is_configured = configured.contains(&(model.provider.clone(), model.id.clone()));
            let is_selected =
                model.provider == resolved.model.provider && model.id == resolved.model.id;
            CodingAgentModelChoice {
                id: model.id,
                name: model.name,
                provider: model.provider,
                reasoning: model.reasoning,
                thinking_capability: model.thinking_capability,
                supports_text: model.supports_text,
                supports_images: model.supports_images,
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                configured: is_configured,
                selected: is_selected,
            }
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        right
            .selected
            .cmp(&left.selected)
            .then_with(|| right.configured.cmp(&left.configured))
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut profiles = profile_registry
        .agents()
        .map(profile_choice_from_agent)
        .chain(profile_registry.teams().map(profile_choice_from_team))
        .collect::<Vec<_>>();
    profiles.sort_by(|left, right| {
        profile_kind_rank(left.kind)
            .cmp(&profile_kind_rank(right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut diagnostics = resolved
        .diagnostics
        .iter()
        .map(embedding_diagnostic_from_application)
        .collect::<Vec<_>>();
    diagnostics.extend(
        profile_registry
            .diagnostics()
            .iter()
            .map(embedding_diagnostic_from_profile),
    );

    CodingAgentEmbeddingSnapshot {
        cwd: resolved.cwd.clone(),
        workspace: options.workspace.clone(),
        global_config_dir: resolved.config_paths.global_dir.clone(),
        selected_model_id: resolved.model.id.clone(),
        default_agent_profile_id: options.default_agent_profile_id.clone(),
        models,
        profiles,
        resources: CodingAgentResourceSummary {
            skill_names: resolved
                .agent_resources
                .skills
                .iter()
                .map(|skill| skill.name.clone())
                .collect(),
            prompt_template_names: resolved
                .agent_resources
                .prompt_templates
                .iter()
                .map(|template| template.name.clone())
                .collect(),
            commands: resource_command_catalog(&resolved.agent_resources),
            context_files: resolved
                .context_files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
        },
        settings: CodingAgentSettingsSummary {
            default_provider: resolved.config.settings.default_provider.clone(),
            default_model: resolved.config.settings.default_model.clone(),
            default_thinking_level: resolved.config.settings.default_thinking_level.clone(),
            session_dir: resolved
                .session
                .as_ref()
                .and_then(|session| session.session_dir.clone()),
            no_context_files: resolved.config.settings.no_context_files,
        },
        diagnostics,
    }
}

pub(crate) fn resource_command_catalog(
    resources: &AgentResources,
) -> Vec<CodingAgentResourceCommand> {
    resources
        .prompt_templates
        .iter()
        .map(|template| CodingAgentResourceCommand {
            name: template.name.clone(),
            command: template.name.clone(),
            description: safe_public_summary(&template.description),
            kind: CodingAgentResourceCommandKind::PromptTemplate,
            model_invocable: false,
        })
        .chain(resources.skills.iter().map(skill_resource_command))
        .collect()
}

pub(super) fn skill_resource_command(skill: &Skill) -> CodingAgentResourceCommand {
    CodingAgentResourceCommand {
        name: skill.name.clone(),
        command: format!("skill:{}", skill.name),
        description: safe_public_summary(&skill.description),
        kind: CodingAgentResourceCommandKind::Skill,
        model_invocable: !skill.disable_model_invocation,
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "embedding tests exercise private catalog helpers declared below"
)]
pub(crate) fn model_catalog_entry(model: &Model) -> CodingAgentModelCatalogEntry {
    CodingAgentModelCatalogEntry {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        reasoning: model.reasoning,
        thinking_capability: thinking_capability(model),
        supports_text: model.input.contains(&ModelInput::Text),
        supports_images: model.input.contains(&ModelInput::Image),
        context_window: model.context_window,
        max_output_tokens: model.max_tokens,
    }
}

const EXPLICIT_THINKING_LEVELS: [CodingAgentThinkingLevel; 5] = [
    CodingAgentThinkingLevel::Minimal,
    CodingAgentThinkingLevel::Low,
    CodingAgentThinkingLevel::Medium,
    CodingAgentThinkingLevel::High,
    CodingAgentThinkingLevel::XHigh,
];

pub(super) fn thinking_capability(model: &Model) -> CodingAgentThinkingCapability {
    if !model.reasoning {
        return CodingAgentThinkingCapability::default();
    }

    CodingAgentThinkingCapability {
        supported: true,
        explicit_levels: if api_supports_explicit_thinking(model) {
            EXPLICIT_THINKING_LEVELS
                .into_iter()
                .filter(|level| thinking_level_is_mapped(model.thinking_level_map.as_ref(), *level))
                .collect()
        } else {
            Vec::new()
        },
        can_disable: api_can_disable_thinking(model),
    }
}

pub(super) fn thinking_level_is_mapped(
    mapping: Option<&ThinkingLevelMap>,
    level: CodingAgentThinkingLevel,
) -> bool {
    let Some(mapping) = mapping else {
        return true;
    };
    let value = match level {
        CodingAgentThinkingLevel::Minimal => mapping.minimal.as_ref(),
        CodingAgentThinkingLevel::Low => mapping.low.as_ref(),
        CodingAgentThinkingLevel::Medium => mapping.medium.as_ref(),
        CodingAgentThinkingLevel::High => mapping.high.as_ref(),
        CodingAgentThinkingLevel::XHigh => mapping.xhigh.as_ref(),
        CodingAgentThinkingLevel::Off => return false,
    };
    !matches!(value, Some(ThinkingLevelValue::Null))
}

pub(super) fn api_supports_explicit_thinking(model: &Model) -> bool {
    match model.api.as_str() {
        "anthropic-messages"
        | "deepseek-responses"
        | "google-generative-ai"
        | "mistral-conversations"
        | "openai-codex-responses"
        | "openai-responses" => true,
        "openai-completions" => openai_completions_compat(model).is_some_and(|compat| {
            compat.supports_reasoning_effort == Some(true)
                || compat.thinking_format == Some(ThinkingFormat::DeepSeek)
        }),
        _ => false,
    }
}

pub(super) fn api_can_disable_thinking(model: &Model) -> bool {
    match model.api.as_str() {
        "anthropic-messages" | "deepseek-responses" | "mistral-conversations" => true,
        "openai-completions" => openai_completions_compat(model)
            .is_some_and(|compat| compat.thinking_format == Some(ThinkingFormat::DeepSeek)),
        _ => false,
    }
}

pub(super) fn openai_completions_compat(
    model: &Model,
) -> Option<&ai::api::compatibility::OpenAICompletionsCompat> {
    match model.compat.as_ref() {
        Some(ModelCompat::OpenAICompletions(compat)) => Some(compat),
        _ => None,
    }
}

pub(crate) fn model_from_catalog_entry(entry: &CodingAgentModelCatalogEntry) -> Option<Model> {
    ai::api::model::get_model(&entry.provider, &entry.id)
}

pub(super) fn profile_choice_from_agent(profile: &AgentProfile) -> CodingAgentProfileChoice {
    CodingAgentProfileChoice {
        id: profile.id.clone(),
        display_name: safe_public_summary(&profile.display_name),
        description: profile.description.as_deref().map(safe_public_summary),
        kind: ProfileKind::Agent,
        source: profile.source,
        model_id: profile.model.clone(),
    }
}

pub(super) fn profile_choice_from_team(profile: &TeamProfile) -> CodingAgentProfileChoice {
    CodingAgentProfileChoice {
        id: profile.id.clone(),
        display_name: safe_public_summary(&profile.display_name),
        description: profile.description.as_deref().map(safe_public_summary),
        kind: ProfileKind::Team,
        source: profile.source,
        model_id: None,
    }
}

const fn profile_kind_rank(kind: ProfileKind) -> u8 {
    match kind {
        ProfileKind::Agent => 0,
        ProfileKind::Team => 1,
    }
}

pub(crate) fn embedding_diagnostic_from_application(
    diagnostic: &ApplicationDiagnostic,
) -> CodingAgentPublicDiagnostic {
    CodingAgentPublicDiagnostic::new(
        match diagnostic.severity {
            ApplicationDiagnosticSeverity::Info => CodingAgentPublicDiagnosticSeverity::Info,
            ApplicationDiagnosticSeverity::Warning => CodingAgentPublicDiagnosticSeverity::Warning,
            ApplicationDiagnosticSeverity::Error => CodingAgentPublicDiagnosticSeverity::Error,
        },
        diagnostic
            .code
            .as_deref()
            .unwrap_or("configuration_diagnostic"),
        &diagnostic.message,
        CodingAgentPublicDiagnosticOrigin::Configuration,
        None,
    )
}

pub(super) fn embedding_diagnostic_from_profile(
    diagnostic: &ProfileDiagnostic,
) -> CodingAgentPublicDiagnostic {
    CodingAgentPublicDiagnostic::from_profile_diagnostic(diagnostic)
}

pub(super) fn default_thinking_level(
    resolved: &ResolvedApplicationContext,
) -> Option<ThinkingLevel> {
    resolved
        .config
        .settings
        .default_thinking_level
        .as_deref()
        .and_then(|level| level.parse().ok())
}
