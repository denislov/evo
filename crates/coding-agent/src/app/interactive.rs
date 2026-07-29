use std::fmt;
use std::ops::Deref;
use std::path::PathBuf;

use agent_core::api::transcript::create_session_id;

use crate::api::error::{CodingAgentPublicDiagnostic, CodingAgentPublicError};
use crate::api::settings::CodingAgentPresentationMode;
use crate::app::auth::CodingAgentAuthController;
use crate::app::bootstrap::{ApplicationRunOptions, PromptInvocation, SessionMode};
use crate::app::embedding::{
    CodingAgentModelCatalogEntry, CodingAgentResourceCommand, CodingAgentThinkingLevel,
    embedding_diagnostic_from_application, model_catalog_entry, resource_command_catalog,
};
use crate::app::error::ApplicationError;
use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::operation_factory::CodingAgentOperationFactory;
use crate::app::profile_catalog::CodingAgentProfileCatalog;
use crate::app::prompt_input::{
    ImageProcessingOptions, ProcessedPromptInput,
    process_at_file_references_with_processing_options,
};
use crate::app::session::{
    CodingAgentSessionBootstrap, CodingAgentSessionChoice, CodingAgentSessionQuery,
    ResolvedSessionTarget,
};
use crate::app::settings::CodingAgentQueueMode;
use crate::app::settings::CodingAgentSettingsController;
use crate::app::startup::{
    ApplicationDiagnostic, ApplicationDiagnosticSeverity, configured_model_choices,
    resolve_application_context_from_options, resolve_profile_registry, rotation_model_choices,
};
use crate::app::theme::{CodingAgentThemeController, CodingAgentThemeSnapshot};
use crate::limits::{
    MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES, MAX_INPUT_IMAGES, MAX_PROMPT_INPUT_BYTES,
};
use crate::operations::prompt::context::QueuedPromptInput;
use crate::runtime::facade::{
    BranchSummaryReusePolicy, CodingAgentOperation, CodingSessionError, ProfileId,
    SelfHealingEditModelRepairOptions, SelfHealingEditRequest,
};

#[derive(Clone)]
struct CodingAgentInteractiveStartupSource {
    invocation: CodingAgentInvocationOptions,
    options: ApplicationRunOptions,
}

/// Product-prepared prompt input for an application adapter.
///
/// The expanded display text is safe for local presentation. Provider content
/// blocks and encoded image payloads remain opaque and can only be consumed by
/// the product operation factory.
#[derive(Clone)]
pub struct CodingAgentPreparedPrompt {
    display_text: String,
    invocation: PromptInvocation,
    attachment_count: usize,
    retained_bytes: usize,
}

impl fmt::Debug for CodingAgentPreparedPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentPreparedPrompt")
            .field("display_text_len", &self.display_text.len())
            .field("attachment_count", &self.attachment_count)
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

impl CodingAgentPreparedPrompt {
    pub fn display_text(&self) -> &str {
        &self.display_text
    }

    pub fn attachment_count(&self) -> usize {
        self.attachment_count
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn into_invocation(self) -> PromptInvocation {
        self.invocation
    }

    pub(crate) fn into_queued_input(self) -> QueuedPromptInput {
        match self.invocation {
            PromptInvocation::Text(text) => QueuedPromptInput::Text(text),
            PromptInvocation::Content(content) => QueuedPromptInput::Content(content),
            _ => unreachable!("prepared application prompt contains only text or content"),
        }
    }
}

/// Bounded image input accepted by product prompt preparation.
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CodingAgentPromptImage {
    data: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

impl fmt::Debug for CodingAgentPromptImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentPromptImage")
            .field("encoded_bytes", &self.data.len())
            .field("mime_type", &self.mime_type)
            .finish()
    }
}

impl CodingAgentPromptImage {
    pub fn new(
        data: impl Into<String>,
        mime_type: impl Into<String>,
    ) -> Result<Self, CodingAgentPublicError> {
        let data = data.into();
        let mime_type = mime_type.into();
        if data.len() > MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES {
            return Err(application_input_error(format!(
                "encoded image exceeds the {MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES} byte safety limit"
            )));
        }
        if mime_type.is_empty() || mime_type.len() > 256 {
            return Err(application_input_error(
                "image MIME type must contain between 1 and 256 bytes",
            ));
        }
        Ok(Self { data, mime_type })
    }

    pub fn encoded_bytes(&self) -> usize {
        self.data.len()
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }
}

/// Product-resolved startup state for an interactive application adapter.
///
/// Provider credentials, executable tools, raw configuration, resource
/// content, theme documents and durable paths stay inside opaque handles.
/// Public fields are bounded presentation values or already-categorized
/// product ports shared by terminal and desktop adapters.
#[derive(Clone)]
pub struct CodingAgentInteractiveStartup {
    source: CodingAgentInteractiveStartupSource,
    pub application: CodingAgentApplicationStartup,
    pub context_file_paths: Vec<PathBuf>,
    pub theme_controller: CodingAgentThemeController,
    pub theme: CodingAgentThemeSnapshot,
    pub terminal_mode: CodingAgentPresentationMode,
    pub model_choices: Vec<CodingAgentModelCatalogEntry>,
    pub model_rotation: Vec<CodingAgentModelCatalogEntry>,
    pub resource_commands: Vec<CodingAgentResourceCommand>,
    pub session_choices: Vec<CodingAgentSessionChoice>,
}

/// Product-resolved runtime bundle shared by application adapters.
///
/// Provider credentials, provider clients, complete settings/resources,
/// executable tools, and durable session paths remain product-owned. Public
/// fields are opaque handles or bounded product facts.
#[derive(Clone)]
pub struct CodingAgentApplicationStartup {
    pub operation_factory: CodingAgentOperationFactory,
    pub auth_controller: CodingAgentAuthController,
    pub settings_controller: CodingAgentSettingsController,
    pub session_bootstrap: CodingAgentSessionBootstrap,
    pub cwd: PathBuf,
    pub thinking_level: Option<CodingAgentThinkingLevel>,
    pub profile_catalog: CodingAgentProfileCatalog,
    pub default_agent_profile_id: ProfileId,
    pub model_summary: CodingAgentModelCatalogEntry,
    model_thinking_level_map: Option<[Option<Option<String>>; 5]>,
    pub session_query: CodingAgentSessionQuery,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

impl fmt::Debug for CodingAgentApplicationStartup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentApplicationStartup")
            .field("cwd", &self.cwd)
            .field("thinking_level", &self.thinking_level)
            .field("default_agent_profile_id", &self.default_agent_profile_id)
            .field("model_summary", &self.model_summary)
            .field("diagnostic_count", &self.diagnostics.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CodingAgentInteractiveStartup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentInteractiveStartup")
            .field("cwd", &self.cwd)
            .field("thinking_level", &self.thinking_level)
            .field("default_agent_profile_id", &self.default_agent_profile_id)
            .field("context_file_count", &self.context_file_paths.len())
            .field("theme", &self.theme)
            .field("terminal_mode", &self.terminal_mode)
            .field("model_summary", &self.model_summary)
            .field("model_choice_count", &self.model_choices.len())
            .field("model_rotation_count", &self.model_rotation.len())
            .field("resource_command_count", &self.resource_commands.len())
            .field("session_choice_count", &self.session_choices.len())
            .field("diagnostic_count", &self.diagnostics.len())
            .finish_non_exhaustive()
    }
}

impl Deref for CodingAgentInteractiveStartup {
    type Target = CodingAgentApplicationStartup;

    fn deref(&self) -> &Self::Target {
        &self.application
    }
}

impl CodingAgentInteractiveStartup {
    /// Resolves one interactive product startup without owning terminal or
    /// process dispatch.
    pub fn resolve(
        cwd: PathBuf,
        invocation: CodingAgentInvocationOptions,
    ) -> Result<Self, CodingAgentPublicError> {
        let options = crate::app::application::default_application_options(
            cwd,
            None,
            SessionMode::Enabled,
            None,
        )
        .map_err(CodingAgentPublicError::from)?;
        Self::from_invocation(invocation, options).map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn from_invocation(
        parsed: CodingAgentInvocationOptions,
        options: ApplicationRunOptions,
    ) -> Result<Self, ApplicationError> {
        build_interactive_startup(parsed, options)
    }

    /// Re-resolve configuration and local resources from the original
    /// invocation while keeping provider/runtime seeds opaque.
    pub fn reload(&self) -> Result<Self, CodingAgentPublicError> {
        build_interactive_startup(self.source.invocation.clone(), self.source.options.clone())
            .map_err(CodingAgentPublicError::from)
    }

    pub fn application(&self) -> &CodingAgentApplicationStartup {
        &self.application
    }

    pub fn into_application(self) -> CodingAgentApplicationStartup {
        self.application
    }
}

impl CodingAgentApplicationStartup {
    /// Resolves the adapter-neutral product runtime used by an application-
    /// owned RPC server.
    pub fn resolve(cwd: PathBuf) -> Result<Self, CodingAgentPublicError> {
        CodingAgentInteractiveStartup::resolve(cwd, CodingAgentInvocationOptions::default())
            .map(CodingAgentInteractiveStartup::into_application)
    }

    /// Returns the selected model's fixed minimal/low/medium/high/xhigh
    /// compatibility mapping without exposing provider model configuration.
    pub fn model_thinking_level_map(&self) -> Option<[Option<Option<String>>; 5]> {
        self.model_thinking_level_map.clone()
    }

    /// Updates connection-local runtime preferences without exposing complete
    /// product settings to the application adapter.
    pub fn configure_runtime_preferences(
        &mut self,
        thinking_level: CodingAgentThinkingLevel,
        steering_mode: CodingAgentQueueMode,
        follow_up_mode: CodingAgentQueueMode,
        auto_compaction_enabled: bool,
    ) {
        self.thinking_level = Some(thinking_level);
        self.operation_factory.configure_runtime_preferences(
            thinking_level,
            steering_mode,
            follow_up_mode,
            auto_compaction_enabled,
        );
    }

    /// Expands and validates a terminal prompt without exposing provider
    /// content blocks or encoded attachments to the application adapter.
    pub fn prepare_prompt(
        &self,
        prompt: &str,
    ) -> Result<CodingAgentPreparedPrompt, CodingAgentPublicError> {
        let settings = self.settings_controller.snapshot();
        let processed = process_at_file_references_with_processing_options(
            prompt,
            &self.cwd,
            ImageProcessingOptions::new(
                settings.runtime.auto_resize_images,
                settings.runtime.block_images,
            ),
        )
        .map_err(CodingAgentPublicError::from)?;
        Ok(prepared_prompt_from_processed(processed))
    }

    /// Validates adapter-supplied text and images and converts them into an
    /// opaque product invocation without exposing provider content blocks.
    pub fn prepare_prompt_with_images(
        &self,
        message: impl Into<String>,
        images: Vec<CodingAgentPromptImage>,
    ) -> Result<CodingAgentPreparedPrompt, CodingAgentPublicError> {
        let message = message.into();
        if message.len() > MAX_PROMPT_INPUT_BYTES {
            return Err(application_input_error(format!(
                "prompt exceeds the {MAX_PROMPT_INPUT_BYTES} byte safety limit"
            )));
        }
        if images.len() > MAX_INPUT_IMAGES {
            return Err(application_input_error(format!(
                "prompt contains more than {MAX_INPUT_IMAGES} images"
            )));
        }
        let Some(image_bytes) = images
            .iter()
            .try_fold(0usize, |total, image| total.checked_add(image.data.len()))
        else {
            return Err(application_input_error("encoded image size overflow"));
        };
        if image_bytes > MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES {
            return Err(application_input_error(format!(
                "encoded images exceed the {MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES} byte aggregate safety limit"
            )));
        }
        if images.is_empty() {
            return Ok(CodingAgentPreparedPrompt {
                retained_bytes: message.len(),
                display_text: message.clone(),
                invocation: PromptInvocation::Text(message),
                attachment_count: 0,
            });
        }

        let attachment_count = images.len();
        let mut display = Vec::with_capacity(images.len() + usize::from(!message.is_empty()));
        let mut content = Vec::with_capacity(images.len() + usize::from(!message.is_empty()));
        if !message.is_empty() {
            display.push(message.clone());
            content.push(ai::api::conversation::ContentBlock::Text {
                text: message.clone(),
                text_signature: None,
            });
        }
        for image in images {
            display.push(format!("[image:{}]", image.mime_type));
            content.push(ai::api::conversation::ContentBlock::Image {
                data: image.data,
                mime_type: image.mime_type,
            });
        }
        Ok(CodingAgentPreparedPrompt {
            display_text: display.join("\n"),
            invocation: PromptInvocation::Content(content),
            attachment_count,
            retained_bytes: message.len().saturating_add(image_bytes),
        })
    }

    pub fn prompt_operation(&self, prompt: CodingAgentPreparedPrompt) -> CodingAgentOperation {
        self.operation_factory
            .prompt_operation(prompt.invocation, self.thinking_level)
    }

    /// Builds one prompt operation with already-validated queued controls.
    pub fn prompt_operation_with_queued_controls(
        &self,
        prompt: CodingAgentPreparedPrompt,
        steering: Vec<CodingAgentPreparedPrompt>,
        follow_up: Vec<CodingAgentPreparedPrompt>,
    ) -> CodingAgentOperation {
        self.operation_factory.prompt_operation_with_queued_inputs(
            prompt.invocation,
            self.thinking_level,
            steering
                .into_iter()
                .map(CodingAgentPreparedPrompt::into_queued_input)
                .collect(),
            follow_up
                .into_iter()
                .map(CodingAgentPreparedPrompt::into_queued_input)
                .collect(),
        )
    }

    pub fn compact_operation(&self, custom_instructions: Option<String>) -> CodingAgentOperation {
        self.operation_factory
            .compact_operation(custom_instructions)
    }

    pub fn agent_invocation_operation(
        &self,
        profile_id: ProfileId,
        task: String,
    ) -> CodingAgentOperation {
        self.operation_factory
            .agent_invocation_operation(profile_id, task, self.thinking_level)
    }

    pub fn team_invocation_operation(
        &self,
        team_id: ProfileId,
        task: String,
    ) -> CodingAgentOperation {
        self.operation_factory
            .team_invocation_operation(team_id, task, self.thinking_level)
    }

    pub fn branch_summary_operation(
        &self,
        source_leaf_id: String,
        target_leaf_id: String,
        custom_instructions: Option<String>,
        reuse: BranchSummaryReusePolicy,
    ) -> CodingAgentOperation {
        self.operation_factory.branch_summary_operation(
            source_leaf_id,
            target_leaf_id,
            custom_instructions,
            reuse,
        )
    }

    pub fn self_healing_edit_operation(
        &self,
        request: SelfHealingEditRequest,
    ) -> CodingAgentOperation {
        self.operation_factory.self_healing_edit_operation(request)
    }

    pub fn fork_session_operation(&self, target_leaf_id: Option<String>) -> CodingAgentOperation {
        self.operation_factory
            .fork_session_operation(target_leaf_id)
    }

    pub fn model_repair_options(&self, max_attempts: usize) -> SelfHealingEditModelRepairOptions {
        self.operation_factory
            .model_repair_options(self.thinking_level, max_attempts)
    }
}

pub(crate) fn prepared_prompt_from_processed(
    processed: ProcessedPromptInput,
) -> CodingAgentPreparedPrompt {
    let attachment_count = processed.images.len();
    let retained_bytes = processed.text.len()
        + processed
            .images
            .iter()
            .map(|image| image.data.len())
            .sum::<usize>();
    let invocation = if processed.images.is_empty() {
        PromptInvocation::Text(processed.text.clone())
    } else {
        PromptInvocation::Content(processed.content)
    };
    CodingAgentPreparedPrompt {
        display_text: processed.text,
        invocation,
        attachment_count,
        retained_bytes,
    }
}

fn application_input_error(message: impl Into<String>) -> CodingAgentPublicError {
    CodingAgentPublicError::from(CodingSessionError::Input {
        message: message.into(),
    })
}

fn build_interactive_startup(
    invocation: CodingAgentInvocationOptions,
    options: ApplicationRunOptions,
) -> Result<CodingAgentInteractiveStartup, ApplicationError> {
    let source = CodingAgentInteractiveStartupSource {
        invocation: invocation.clone(),
        options: options.clone(),
    };
    let mut resolved = resolve_application_context_from_options(invocation.clone(), options)?;
    let diagnostics = resolved
        .diagnostics
        .iter()
        .map(embedding_diagnostic_from_application)
        .collect();
    let model_rotation = rotation_model_choices(
        invocation.model_rotation.as_deref(),
        invocation
            .provider
            .as_deref()
            .or(resolved.config.settings.default_provider.as_deref()),
        Some(&resolved.config.settings.enabled_models),
    )?
    .iter()
    .map(model_catalog_entry)
    .collect();
    let model_choices = configured_model_choices(
        &resolved.model,
        invocation.api_key.as_deref(),
        &resolved.config.auth,
    )
    .iter()
    .map(model_catalog_entry)
    .collect();
    let model_summary = model_catalog_entry(&resolved.model);
    let model_thinking_level_map = model_thinking_level_map(&resolved.model);

    let session_target = match (&resolved.session, resolved.session_target.clone()) {
        (Some(session), None) if matches!(session.mode, SessionMode::Enabled) => {
            Some(ResolvedSessionTarget::OpenOrCreateId(create_session_id()))
        }
        (_, target) => target,
    };
    let cwd = resolved
        .session
        .as_ref()
        .map(|session| session.cwd.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    let session_query = CodingAgentSessionQuery::from_run_options(&resolved.session)
        .map_err(ApplicationError::from)?;
    let mut session_choices = match session_query.catalog() {
        Ok(catalog) => catalog.choices,
        Err(error) => {
            resolved.diagnostics.push(ApplicationDiagnostic {
                severity: ApplicationDiagnosticSeverity::Warning,
                message: format!("failed to list sessions: {error}"),
                source: None,
                code: Some("session_catalog".into()),
            });
            Vec::new()
        }
    };
    session_choices.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let profile_registry = resolve_profile_registry(&resolved)?;
    let default_agent_profile_id = ProfileId::from("default");
    let profile_catalog =
        CodingAgentProfileCatalog::from_registry(&profile_registry, &default_agent_profile_id);
    let resource_commands = resource_command_catalog(&resolved.agent_resources);
    let auth_controller = CodingAgentAuthController::from_internal(
        cwd.clone(),
        invocation.api_key.clone(),
        resolved.config.auth.clone(),
    );
    let settings_controller =
        CodingAgentSettingsController::from_internal(cwd.clone(), resolved.config.settings.clone());
    let session_bootstrap = CodingAgentSessionBootstrap::from_internal(
        resolved.session.clone(),
        session_target,
        resolved.session_name.clone(),
        default_agent_profile_id.clone(),
    );
    let thinking_level = invocation.thinking.or_else(|| {
        resolved
            .config
            .settings
            .default_thinking_level
            .as_deref()
            .and_then(|value| value.parse::<CodingAgentThinkingLevel>().ok())
    });
    let operation_factory = CodingAgentOperationFactory::from_runtime_parts(
        resolved.model.clone(),
        resolved.api_key.clone(),
        resolved.auth_diagnostics.clone(),
        resolved.system_prompt.clone(),
        invocation.max_turns,
        resolved.tools.clone(),
        resolved.register_builtins,
        resolved.ai_client.clone(),
        resolved.session.clone(),
        thinking_level,
        invocation.tool_execution.map(Into::into),
        resolved.agent_resources.clone(),
        Some(resolved.config.settings.clone()),
        resolved.session_name,
        default_agent_profile_id.clone(),
    );
    let theme_controller =
        CodingAgentThemeController::from_internal(resolved.config_paths.global_dir.join("themes"));
    let theme = theme_controller.initial_snapshot(
        resolved.config.settings.theme.as_deref(),
        resolved.loaded_resources.selected_theme.as_ref(),
    );
    let terminal_mode = invocation
        .presentation_mode
        .unwrap_or_else(|| settings_controller.snapshot().presentation.mode);

    let application = CodingAgentApplicationStartup {
        operation_factory,
        auth_controller,
        settings_controller,
        session_bootstrap,
        cwd,
        thinking_level,
        profile_catalog,
        default_agent_profile_id,
        model_summary,
        model_thinking_level_map,
        session_query,
        diagnostics,
    };

    Ok(CodingAgentInteractiveStartup {
        source,
        application,
        context_file_paths: resolved
            .context_files
            .into_iter()
            .map(|context| context.path)
            .collect(),
        theme_controller,
        theme,
        terminal_mode,
        model_choices,
        model_rotation,
        resource_commands,
        session_choices,
    })
}

fn model_thinking_level_map(model: &ai::api::model::Model) -> Option<[Option<Option<String>>; 5]> {
    let mapping = serde_json::to_value(model.thinking_level_map.as_ref()?)
        .expect("provider thinking-level mapping serializes");
    let mapping = mapping
        .as_object()
        .expect("provider thinking-level mapping serializes as an object");
    Some(["minimal", "low", "medium", "high", "xhigh"].map(|name| {
        mapping.get(name).map(|value| match value {
            serde_json::Value::Null => None,
            serde_json::Value::String(value) => Some(value.clone()),
            _ => unreachable!("provider thinking-level mapping values are string or null"),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_omits_private_startup_seed() {
        let secret = "interactive-startup-secret";
        let startup = CodingAgentInteractiveStartup::from_invocation(
            CodingAgentInvocationOptions {
                api_key: Some(secret.into()),
                ..CodingAgentInvocationOptions::default()
            },
            ApplicationRunOptions::default(),
        )
        .unwrap();
        let debug = format!("{startup:?}");

        assert!(!debug.contains("startup_source"));
        assert!(!debug.contains("api_key"));
        assert!(!debug.contains("ai_client"));
        assert!(!debug.contains(secret));
    }

    #[test]
    fn prepared_prompt_keeps_provider_content_opaque() {
        let startup = CodingAgentInteractiveStartup::from_invocation(
            CodingAgentInvocationOptions::default(),
            ApplicationRunOptions::default(),
        )
        .unwrap();
        let prepared = startup.prepare_prompt("hello").unwrap();

        assert_eq!(prepared.display_text(), "hello");
        assert_eq!(prepared.attachment_count(), 0);
        assert!(!format!("{prepared:?}").contains("hello"));
        let CodingAgentOperation::Prompt(options) = startup.prompt_operation(prepared) else {
            panic!("prepared prompt must construct a prompt operation");
        };
        assert!(matches!(
            options.invocation(),
            PromptInvocation::Text(text) if text == "hello"
        ));
    }

    #[test]
    fn application_startup_debug_omits_private_runtime_seed() {
        let secret = "application-startup-secret";
        let startup = CodingAgentInteractiveStartup::from_invocation(
            CodingAgentInvocationOptions {
                api_key: Some(secret.into()),
                ..CodingAgentInvocationOptions::default()
            },
            ApplicationRunOptions::default(),
        )
        .unwrap()
        .into_application();

        let debug = format!("{startup:?}");
        assert!(!debug.contains("api_key"));
        assert!(!debug.contains("ai_client"));
        assert!(!debug.contains(secret));
    }

    #[test]
    fn application_multimodal_prompt_keeps_image_payload_opaque() {
        let secret = "opaque-image-payload";
        let startup = CodingAgentInteractiveStartup::from_invocation(
            CodingAgentInvocationOptions::default(),
            ApplicationRunOptions::default(),
        )
        .unwrap()
        .into_application();
        let image = CodingAgentPromptImage::new(secret, "image/png").unwrap();

        assert!(!format!("{image:?}").contains(secret));
        let prompt = startup
            .prepare_prompt_with_images("describe", vec![image])
            .unwrap();
        assert_eq!(prompt.display_text(), "describe\n[image:image/png]");
        assert_eq!(prompt.attachment_count(), 1);
        assert_eq!(prompt.retained_bytes(), "describe".len() + secret.len());
        assert!(!format!("{prompt:?}").contains(secret));
    }
}
