use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use agent_core::api::agent::ThinkingLevel;
use agent_core::api::resources::{AgentResources, Skill, load_skills};
use ai::api::compatibility::{ModelCompat, ThinkingFormat, ThinkingLevelMap, ThinkingLevelValue};
use ai::api::model::{Model, ModelInput};

use crate::app::auth::{CodingAgentAuthController, load_global_auth_store};
use crate::app::bootstrap::{DEFAULT_MODEL_ID, PromptInvocation, SessionMode, select_model};
use crate::app::interactive::{CodingAgentPreparedPrompt, prepared_prompt_from_processed};
use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::operation_factory::CodingAgentOperationFactory;
use crate::app::profile_catalog::CodingAgentProfileCatalog;
use crate::app::prompt_input::{ImageProcessingOptions, process_explicit_file_attachments};
use crate::app::session::{
    CodingAgentSessionBootstrap, CodingAgentSessionQuery, runtime_session_root,
};
use crate::app::settings::{CodingAgentSettingsController, load_global_settings_state};
use crate::app::startup::{
    ApplicationDiagnostic, ApplicationDiagnosticSeverity, ResolvedApplicationContext,
    configured_model_choices, resolve_application_context_from_options, resolve_profile_registry,
};
use crate::authorization::ToolAuthorizationMode;
use crate::profiles::{
    AgentProfile, ProfileDiagnostic, ProfileId, ProfileKind, ProfileRegistry, ProfileSource,
    TeamProfile,
};
use crate::runtime::facade::{
    CodingAgentOperation, CodingAgentPublicDiagnostic, CodingAgentPublicDiagnosticOrigin,
    CodingAgentPublicDiagnosticSeverity, CodingAgentPublicError, CodingAgentSession,
    CodingAgentSessionOptions, CodingAgentSessionSummary, CodingSessionError,
};
use crate::runtime::public_error::safe_public_summary;
use crate::workspace::{
    CodingAgentResolvedWorkspace, CodingAgentWorkspaceResolutionError, CodingAgentWorkspaceScope,
    CodingAgentWorkspaceSelection,
};

/// Product-owned options for loading one embeddable project context.
///
/// Credential values are resolved internally and never exposed by this type or
/// by [`CodingAgentEmbeddingContext`].
#[derive(Debug, Clone)]
pub struct CodingAgentEmbeddingOptions {
    cwd: PathBuf,
    workspace: Option<CodingAgentResolvedWorkspace>,
    session_mode: SessionMode,
    session_dir: Option<PathBuf>,
    model_id: Option<String>,
    default_agent_profile_id: ProfileId,
    global_config_only: bool,
}

impl CodingAgentEmbeddingOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            workspace: None,
            session_mode: SessionMode::Enabled,
            session_dir: None,
            model_id: None,
            default_agent_profile_id: ProfileId::from("default"),
            global_config_only: false,
        }
    }

    /// Resolve a typed workspace selection into options for one immutable
    /// workspace context.
    ///
    /// Project contexts load project configuration and resources. Projectless
    /// contexts use only user-global state while executing in a managed scratch
    /// directory. Both freeze the same product-global durable session root so a
    /// project's local settings cannot redirect session persistence.
    pub fn for_workspace(
        selection: CodingAgentWorkspaceSelection,
    ) -> Result<Self, CodingAgentWorkspaceResolutionError> {
        let global_config_dir = global_config_directory();
        let workspace = selection.resolve(&global_config_dir)?;
        let global_config_only = matches!(
            &workspace.scope,
            CodingAgentWorkspaceScope::Projectless { .. }
        );
        let cwd = workspace.execution_cwd.clone();
        Ok(Self {
            cwd,
            workspace: Some(workspace),
            session_mode: SessionMode::Enabled,
            session_dir: Some(workspace_session_root(&global_config_dir)),
            model_id: None,
            default_agent_profile_id: ProfileId::from("default"),
            global_config_only,
        })
    }

    pub fn with_session_mode(mut self, mode: SessionMode) -> Self {
        self.session_mode = mode;
        self
    }

    pub fn with_session_dir(mut self, session_dir: impl Into<PathBuf>) -> Self {
        self.session_dir = Some(session_dir.into());
        self
    }

    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    pub fn with_default_agent_profile_id(mut self, profile_id: impl Into<ProfileId>) -> Self {
        self.default_agent_profile_id = profile_id.into();
        self
    }

    /// Resolve user-global configuration and resources without consulting the
    /// working directory's `.evo` tree or ancestor context files.
    pub fn with_global_config_only(mut self) -> Self {
        self.global_config_only = true;
        self
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn workspace(&self) -> Option<&CodingAgentResolvedWorkspace> {
        self.workspace.as_ref()
    }

    pub fn session_mode(&self) -> SessionMode {
        self.session_mode.clone()
    }

    pub fn session_dir(&self) -> Option<&Path> {
        self.session_dir.as_deref()
    }

    pub fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    pub fn default_agent_profile_id(&self) -> &ProfileId {
        &self.default_agent_profile_id
    }

    pub fn global_config_only(&self) -> bool {
        self.global_config_only
    }
}

/// Return the product-resolved root for user-global configuration.
pub fn global_config_directory() -> PathBuf {
    crate::config::resolve_paths(Path::new(".")).global_dir
}

fn workspace_session_root(global_config_dir: &Path) -> PathBuf {
    load_global_settings_state()
        .session_dir
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("EVO_SESSION_DIR").map(PathBuf::from))
        .unwrap_or_else(|| global_config_dir.join("sessions"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentModelChoice {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub reasoning: bool,
    pub thinking_capability: CodingAgentThinkingCapability,
    pub supports_text: bool,
    pub supports_images: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub configured: bool,
    pub selected: bool,
}

/// One safe, immutable model-catalog entry for product adapters.
///
/// The catalog intentionally omits provider transport configuration,
/// credentials, headers, compatibility payloads, and pricing internals.
/// Adapter-local filtering and presentation can use these fields without a
/// direct dependency on `ai`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentModelCatalogEntry {
    pub id: String,
    pub name: String,
    pub api: String,
    pub provider: String,
    pub reasoning: bool,
    pub thinking_capability: CodingAgentThinkingCapability,
    pub supports_text: bool,
    pub supports_images: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
}

/// Provider-neutral thinking controls supported by one model.
///
/// `supported` distinguishes reasoning models that only support Auto from
/// non-reasoning models that should hide the thinking selector entirely.
/// `explicit_levels` excludes [`CodingAgentThinkingLevel::Off`]; callers use
/// `can_disable` to decide whether an explicit Off choice is legal. An empty
/// level list means the model has no user-selectable thinking intensity, even
/// when the underlying catalog marks it as a reasoning model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingAgentThinkingCapability {
    pub supported: bool,
    pub explicit_levels: Vec<CodingAgentThinkingLevel>,
    pub can_disable: bool,
}

impl CodingAgentThinkingCapability {
    pub fn supports(&self, level: CodingAgentThinkingLevel) -> bool {
        match level {
            CodingAgentThinkingLevel::Off => self.can_disable,
            _ => self.explicit_levels.contains(&level),
        }
    }
}

/// Result of reconciling an explicit thinking request with a model catalog
/// capability. Unsupported requests fall back to product-controlled Auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentThinkingLevelSanitization {
    Explicit(CodingAgentThinkingLevel),
    AutoFallback,
}

/// Keep a supported explicit level or return an Auto fallback without exposing
/// provider compatibility details to application adapters.
pub fn sanitize_thinking_level(
    model: &CodingAgentModelChoice,
    requested: CodingAgentThinkingLevel,
) -> CodingAgentThinkingLevelSanitization {
    if model.thinking_capability.supports(requested) {
        CodingAgentThinkingLevelSanitization::Explicit(requested)
    } else {
        CodingAgentThinkingLevelSanitization::AutoFallback
    }
}

/// Return the stable, product-owned model catalog in provider/id order.
///
/// This query is configuration- and session-independent so discovery commands
/// remain read-only and do not need to construct a runtime or resolve
/// credentials.
pub fn model_catalog() -> Vec<CodingAgentModelCatalogEntry> {
    let mut models = ai::api::model::all_models()
        .iter()
        .map(model_catalog_entry)
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.id.cmp(&right.id))
    });
    models
}

/// Resolve one model id using the product's deterministic provider priority.
///
/// Multiple providers may publish the same model id. Adapters must not derive
/// provider priority from catalog presentation order.
pub fn model_catalog_entry_by_id(model_id: &str) -> Option<CodingAgentModelCatalogEntry> {
    ai::api::model::lookup_model(model_id)
        .as_ref()
        .map(model_catalog_entry)
}

/// Return models whose providers have credentials in the user-global auth
/// configuration (or the provider's supported environment variables).
///
/// The global auth store is consumed internally. Returned entries contain no
/// credential material, provider headers, or transport configuration.
pub fn configured_model_catalog() -> Vec<CodingAgentModelCatalogEntry> {
    let settings = load_global_settings_state();
    let current_model = select_model(
        &CodingAgentInvocationOptions::default(),
        settings.default_provider.as_deref(),
        settings.default_model.as_deref(),
        None,
    )
    .ok()
    .or_else(|| ai::api::model::lookup_model(DEFAULT_MODEL_ID));
    let Some(current_model) = current_model else {
        return Vec::new();
    };
    configured_model_choices(&current_model, None, &load_global_auth_store())
        .iter()
        .map(model_catalog_entry)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentProfileChoice {
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub kind: ProfileKind,
    pub source: ProfileSource,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentResourceSummary {
    pub skill_names: Vec<String>,
    pub prompt_template_names: Vec<String>,
    pub commands: Vec<CodingAgentResourceCommand>,
    pub context_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentResourceCommandKind {
    Skill,
    PromptTemplate,
}

/// Safe adapter-facing description of one product resource command.
///
/// Resource content and filesystem locations remain private to the product
/// runtime. Adapters retain only presentation metadata and can construct the
/// typed invocation accepted by the product operation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentResourceCommand {
    pub name: String,
    pub command: String,
    pub description: String,
    pub kind: CodingAgentResourceCommandKind,
    pub model_invocable: bool,
}

impl CodingAgentResourceCommand {
    pub fn prompt_invocation(&self, arguments: &str) -> PromptInvocation {
        match self.kind {
            CodingAgentResourceCommandKind::Skill => {
                let additional_instructions = arguments.trim();
                PromptInvocation::Skill {
                    name: self.name.clone(),
                    additional_instructions: (!additional_instructions.is_empty())
                        .then(|| additional_instructions.to_string()),
                }
            }
            CodingAgentResourceCommandKind::PromptTemplate => PromptInvocation::PromptTemplate {
                name: self.name.clone(),
                args: agent_core::api::resources::parse_command_args(arguments),
            },
        }
    }
}

/// Return the safe command catalog for user-global skills only.
///
/// This reads `<global-config>/skills` and intentionally ignores the current
/// project, project `.evo/skills`, and every configured `skills_dirs` path.
/// Skill bodies and filesystem locations are not exposed.
pub fn global_skill_catalog() -> Vec<CodingAgentResourceCommand> {
    let global_skills = crate::config::resolve_paths(Path::new("."))
        .global_dir
        .join("skills");
    let (skills, _) = load_skills(&[global_skills]);
    let mut catalog = skills
        .iter()
        .map(skill_resource_command)
        .collect::<Vec<_>>();
    catalog.sort_by(|left, right| left.name.cmp(&right.name));
    catalog
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSettingsSummary {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<String>,
    pub session_dir: Option<PathBuf>,
    pub no_context_files: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentEmbeddingSnapshot {
    pub cwd: PathBuf,
    /// Typed workspace identity and execution cwd for scope-aware contexts.
    ///
    /// `None` is retained only for callers still using the transitional raw
    /// [`CodingAgentEmbeddingOptions::new`] constructor.
    pub workspace: Option<CodingAgentResolvedWorkspace>,
    /// Product-resolved root for client-local adapter state.
    ///
    /// This is path information only: it grants no authority over product
    /// settings, credentials, registries, or session truth.
    pub global_config_dir: PathBuf,
    pub selected_model_id: String,
    pub default_agent_profile_id: ProfileId,
    pub models: Vec<CodingAgentModelChoice>,
    pub profiles: Vec<CodingAgentProfileChoice>,
    pub resources: CodingAgentResourceSummary,
    pub settings: CodingAgentSettingsSummary,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

/// Reloadable product context for non-CLI adapters.
///
/// The context owns resolved credentials and complete resource content
/// privately. Its `Debug` output is intentionally restricted to the safe
/// snapshot. Reload is transactional: a load failure leaves the previous
/// context unchanged.
pub struct CodingAgentEmbeddingContext {
    options: CodingAgentEmbeddingOptions,
    resolved: ResolvedApplicationContext,
    profile_registry: ProfileRegistry,
    snapshot: CodingAgentEmbeddingSnapshot,
}

impl fmt::Debug for CodingAgentEmbeddingContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentEmbeddingContext")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl CodingAgentEmbeddingContext {
    pub fn load(options: CodingAgentEmbeddingOptions) -> Result<Self, CodingAgentPublicError> {
        Self::load_internal(options).map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn load_internal(
        options: CodingAgentEmbeddingOptions,
    ) -> Result<Self, CodingSessionError> {
        let model_override = options.model_id().map(resolve_model).transpose()?;
        let mut run_options = crate::app::application::default_application_options(
            options.cwd.clone(),
            model_override,
            options.session_mode.clone(),
            options.session_dir.clone(),
        )?;
        run_options.global_config_only =
            options
                .workspace
                .as_ref()
                .map_or(options.global_config_only, |workspace| {
                    matches!(
                        &workspace.scope,
                        CodingAgentWorkspaceScope::Projectless { .. }
                    )
                });
        run_options.session.workspace = options.workspace.clone();
        let mut resolved = resolve_application_context_from_options(
            CodingAgentInvocationOptions::default(),
            run_options,
        )
        .map_err(|error| CodingSessionError::Config {
            message: error.to_string(),
        })?;
        if options.workspace.is_some()
            && let Some(session) = resolved.session.as_mut()
        {
            session.session_dir = options.session_dir.clone();
        }
        let profile_registry =
            resolve_profile_registry(&resolved).map_err(|error| CodingSessionError::Config {
                message: error.to_string(),
            })?;
        validate_profile(&profile_registry, options.default_agent_profile_id())?;
        let snapshot = build_snapshot(&options, &resolved, &profile_registry);
        Ok(Self {
            options,
            resolved,
            profile_registry,
            snapshot,
        })
    }

    pub fn snapshot(&self) -> &CodingAgentEmbeddingSnapshot {
        &self.snapshot
    }

    pub fn reload_local_resources(
        &mut self,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingAgentPublicError> {
        self.reload_local_resources_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn reload_local_resources_internal(
        &mut self,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingSessionError> {
        let replacement = Self::load_internal(self.options.clone())?;
        *self = replacement;
        Ok(&self.snapshot)
    }

    pub fn select_model(
        &mut self,
        model_id: impl Into<String>,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingAgentPublicError> {
        self.select_model_internal(model_id)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn select_model_internal(
        &mut self,
        model_id: impl Into<String>,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingSessionError> {
        let mut options = self.options.clone();
        options.model_id = Some(model_id.into());
        let replacement = Self::load_internal(options)?;
        *self = replacement;
        Ok(&self.snapshot)
    }

    pub fn select_default_agent_profile(
        &mut self,
        profile_id: impl Into<ProfileId>,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingAgentPublicError> {
        self.select_default_agent_profile_internal(profile_id)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn select_default_agent_profile_internal(
        &mut self,
        profile_id: impl Into<ProfileId>,
    ) -> Result<&CodingAgentEmbeddingSnapshot, CodingSessionError> {
        let mut options = self.options.clone();
        options.default_agent_profile_id = profile_id.into();
        let replacement = Self::load_internal(options)?;
        *self = replacement;
        Ok(&self.snapshot)
    }

    pub fn prompt_operation(
        &self,
        invocation: PromptInvocation,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> CodingAgentOperation {
        self.operation_factory()
            .prompt_operation(invocation, thinking_level)
    }

    /// Validates explicit adapter-selected files through the product-owned
    /// `@file` pipeline and returns an opaque prompt invocation.
    pub fn prepare_prompt_with_attachments(
        &self,
        prompt: &str,
        attachments: &[PathBuf],
    ) -> Result<CodingAgentPreparedPrompt, CodingAgentPublicError> {
        process_explicit_file_attachments(
            prompt,
            attachments,
            &self.options.cwd,
            ImageProcessingOptions::from_settings(&self.resolved.config.settings),
        )
        .map(prepared_prompt_from_processed)
        .map_err(CodingAgentPublicError::from)
    }

    /// Consumes an opaque prepared prompt through the product operation
    /// factory, without exposing provider content to the embedding adapter.
    pub fn prepared_prompt_operation(
        &self,
        prompt: CodingAgentPreparedPrompt,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> CodingAgentOperation {
        self.prompt_operation(prompt.into_invocation(), thinking_level)
    }

    /// Return an opaque product-owned operation factory for this resolved
    /// project context.
    ///
    /// The handle retains provider credentials, executable tools, complete
    /// resource content, and lower-runtime configuration privately.
    pub fn operation_factory(&self) -> CodingAgentOperationFactory {
        CodingAgentOperationFactory::from_runtime_parts(
            self.resolved.model.clone(),
            self.resolved.api_key.clone(),
            self.resolved.auth_diagnostics.clone(),
            self.resolved.system_prompt.clone(),
            None,
            self.resolved.tools.clone(),
            self.resolved.register_builtins,
            self.resolved.ai_client.clone(),
            self.resolved.session.clone(),
            default_thinking_level(&self.resolved).map(Into::into),
            None,
            self.resolved.agent_resources.clone(),
            Some(self.resolved.config.settings.clone()),
            self.resolved.session_name.clone(),
            self.options.default_agent_profile_id.clone(),
        )
    }

    /// Build an opaque provider-authentication controller for this context.
    ///
    /// Credential values and auth-file paths remain private. Commands update
    /// an opaque operation factory in place so adapters never receive resolved
    /// provider secrets.
    pub fn auth_controller(&self) -> CodingAgentAuthController {
        CodingAgentAuthController::from_internal(
            self.options.cwd.clone(),
            None,
            self.resolved.config.auth.clone(),
        )
    }

    /// Build an opaque product-settings controller for this context.
    ///
    /// Complete runtime settings and configuration paths remain private. The
    /// controller exposes only bounded adapter snapshots and typed mutations.
    pub fn settings_controller(&self) -> CodingAgentSettingsController {
        CodingAgentSettingsController::from_internal(
            self.options.cwd.clone(),
            self.resolved.config.settings.clone(),
        )
    }

    /// Return the bounded agent/team presentation catalog for this context.
    ///
    /// Profile files, system prompts, complete delegation policy, and registry
    /// lookup authority remain private.
    pub fn profile_catalog(&self) -> CodingAgentProfileCatalog {
        CodingAgentProfileCatalog::from_registry(
            &self.profile_registry,
            self.options.default_agent_profile_id(),
        )
    }

    pub fn session_options(&self) -> Result<CodingAgentSessionOptions, CodingAgentPublicError> {
        self.session_options_internal()
            .map_err(CodingAgentPublicError::from)
    }

    /// Build the bounded durable-session navigation port for this context.
    ///
    /// Repository roots and session directories remain private inside the
    /// returned handle; adapters address sessions only by product identity.
    pub fn session_query(&self) -> Result<CodingAgentSessionQuery, CodingAgentPublicError> {
        CodingAgentSessionQuery::from_run_options(&self.resolved.session)
            .map_err(CodingAgentPublicError::from)
    }

    /// Build an opaque session bootstrap handle for this context.
    pub fn session_bootstrap(&self) -> CodingAgentSessionBootstrap {
        CodingAgentSessionBootstrap::from_internal(
            self.resolved.session.clone(),
            None,
            self.resolved.session_name.clone(),
            self.options.default_agent_profile_id.clone(),
        )
    }

    pub(crate) fn session_options_internal(
        &self,
    ) -> Result<CodingAgentSessionOptions, CodingSessionError> {
        let options = match self.options.workspace.as_ref() {
            Some(workspace) => {
                CodingAgentSessionOptions::new().with_resolved_workspace(workspace.clone())
            }
            None => CodingAgentSessionOptions::new().with_cwd(self.options.cwd.clone()),
        };
        let mut options = options
            .with_default_agent_profile_id(self.options.default_agent_profile_id.clone())
            .with_tool_authorization_mode(ToolAuthorizationMode::Interactive);
        if let Some(root) = self
            .resolved
            .session
            .as_ref()
            .map(runtime_session_root)
            .transpose()?
            .flatten()
        {
            options = options.with_session_log_root(root);
        }
        if let Some(name) = self.resolved.session_name.as_deref() {
            options = options.with_session_name(name);
        }
        Ok(options)
    }

    pub fn list_sessions(&self) -> Result<Vec<CodingAgentSessionSummary>, CodingAgentPublicError> {
        self.list_sessions_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn list_sessions_internal(
        &self,
    ) -> Result<Vec<CodingAgentSessionSummary>, CodingSessionError> {
        if !self.sessions_are_persistent() {
            return Ok(Vec::new());
        }
        CodingAgentSession::list_internal(self.session_options_internal()?)
    }

    pub async fn create_session(&self) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.create_session_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    /// Create a new persistent session with a caller-assigned product id.
    ///
    /// The id is normalized and validated by the product session repository.
    /// This is create-only: an existing id returns a typed session error rather
    /// than opening or replacing the existing session.
    pub async fn create_session_with_id(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.create_session_with_id_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn create_session_internal(
        &self,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        let options = self.session_options_internal()?;
        if self.sessions_are_persistent() {
            CodingAgentSession::create_internal(options).await
        } else {
            CodingAgentSession::non_persistent_internal(options).await
        }
    }

    pub(crate) async fn create_session_with_id_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::create_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    pub async fn open_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.open_session_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_session_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::open_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    pub async fn open_or_create_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.open_or_create_session_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_or_create_session_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::open_or_create_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    fn sessions_are_persistent(&self) -> bool {
        self.resolved
            .session
            .as_ref()
            .is_some_and(|session| matches!(session.mode, SessionMode::Enabled))
    }

    fn require_persistent_sessions(&self) -> Result<(), CodingSessionError> {
        if self.sessions_are_persistent() {
            Ok(())
        } else {
            Err(CodingSessionError::UnsupportedCapability {
                capability: "opening a named session while persistence is disabled".into(),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodingAgentThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

impl fmt::Display for CodingAgentThinkingLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        })
    }
}

impl std::str::FromStr for CodingAgentThinkingLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            _ => Err(format!("unknown thinking level: {value}")),
        }
    }
}

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

fn resolve_model(model_id: &str) -> Result<Model, CodingSessionError> {
    ai::api::model::lookup_model(model_id).ok_or_else(|| CodingSessionError::Config {
        message: format!("unknown model: {model_id}"),
    })
}

fn validate_profile(
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

fn build_snapshot(
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

fn skill_resource_command(skill: &Skill) -> CodingAgentResourceCommand {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::auth::{CodingAgentProviderAuthKind, global_auth_snapshot};
    use crate::app::settings::global_settings_snapshot;
    use crate::config::AuthStore;
    use crate::runtime::facade::CodingAgentErrorCategory;
    use crate::workspace::CodingAgentWorkspaceKind;
    use agent_core::api::tool::ToolExecutionContext;
    use ai::api::compatibility::{
        AnthropicMessagesCompat, OpenAICompletionsCompat, ThinkingLevelValue,
    };
    use ai::api::conversation::ContentBlock;
    use ai::api::model::ModelCost;

    fn thinking_model(api: &str, reasoning: bool) -> Model {
        Model {
            id: format!("{api}-test-model"),
            name: "Thinking Test Model".into(),
            api: api.into(),
            provider: "test-provider".into(),
            base_url: String::new(),
            reasoning,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 128_000,
            max_tokens: 16_000,
            headers: None,
            compat: None,
        }
    }

    fn write_test_skill(root: &Path, name: &str) {
        let directory = root.join("skills").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} description\n---\n{name} body\n"),
        )
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_contexts_isolate_project_state_and_share_the_global_session_root() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR", "EVO_SESSION_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let shared_sessions = temp.path().join("shared-sessions");
        let project_a = temp.path().join("project-a");
        let project_b = temp.path().join("project-b");
        std::fs::create_dir_all(global.as_path()).unwrap();
        std::fs::create_dir_all(project_a.join(".evo/agents")).unwrap();
        std::fs::create_dir_all(project_b.join(".evo")).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            format!(
                "session_dir = {}\n",
                toml::Value::String(shared_sessions.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        std::fs::write(
            project_a.join(".evo/settings.toml"),
            format!(
                "default_thinking_level = \"low\"\nsession_dir = {}\n",
                toml::Value::String(
                    temp.path()
                        .join("project-a-sessions")
                        .to_string_lossy()
                        .into_owned()
                )
            ),
        )
        .unwrap();
        std::fs::write(
            project_b.join(".evo/settings.toml"),
            format!(
                "default_thinking_level = \"high\"\nsession_dir = {}\n",
                toml::Value::String(
                    temp.path()
                        .join("project-b-sessions")
                        .to_string_lossy()
                        .into_owned()
                )
            ),
        )
        .unwrap();
        std::fs::write(project_a.join("AGENTS.md"), "project a context").unwrap();
        std::fs::write(project_b.join("AGENTS.md"), "project b context").unwrap();
        write_test_skill(&project_a.join(".evo"), "project-a-skill");
        write_test_skill(&project_b.join(".evo"), "project-b-skill");
        std::fs::write(
            project_a.join(".evo/agents/project-a.toml"),
            "schema_version = 1\nid = \"project-a\"\ndisplay_name = \"Project A\"\n",
        )
        .unwrap();
        env.set_evo_dir(&global);

        let context_a = CodingAgentEmbeddingContext::load(
            CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(
                &project_a,
            ))
            .unwrap()
            .with_model_id("gpt-5")
            .with_default_agent_profile_id("project-a"),
        )
        .unwrap();
        let context_b = CodingAgentEmbeddingContext::load(
            CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(
                &project_b,
            ))
            .unwrap(),
        )
        .unwrap();

        let canonical_a = project_a.canonicalize().unwrap();
        let canonical_b = project_b.canonicalize().unwrap();
        assert_eq!(context_a.snapshot().cwd, canonical_a);
        assert_eq!(context_b.snapshot().cwd, canonical_b);
        assert_eq!(
            context_a
                .snapshot()
                .workspace
                .as_ref()
                .map(|workspace| &workspace.scope),
            Some(&CodingAgentWorkspaceScope::Project {
                cwd: canonical_a.clone(),
            })
        );
        assert_eq!(context_a.snapshot().selected_model_id, "gpt-5");
        assert_eq!(
            context_a.snapshot().default_agent_profile_id.as_str(),
            "project-a"
        );
        assert!(
            context_a
                .snapshot()
                .profiles
                .iter()
                .any(|profile| profile.id.as_str() == "project-a")
        );
        assert!(
            !context_b
                .snapshot()
                .profiles
                .iter()
                .any(|profile| profile.id.as_str() == "project-a")
        );
        assert_eq!(
            context_a
                .snapshot()
                .settings
                .default_thinking_level
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            context_b
                .snapshot()
                .settings
                .default_thinking_level
                .as_deref(),
            Some("high")
        );
        assert!(
            context_a
                .snapshot()
                .resources
                .skill_names
                .iter()
                .any(|name| name == "project-a-skill")
        );
        assert!(
            !context_a
                .snapshot()
                .resources
                .skill_names
                .iter()
                .any(|name| name == "project-b-skill")
        );
        assert!(
            context_a
                .snapshot()
                .resources
                .context_files
                .contains(&canonical_a.join("AGENTS.md"))
        );
        assert!(
            !context_a
                .snapshot()
                .resources
                .context_files
                .contains(&canonical_b.join("AGENTS.md"))
        );

        let bash = context_a
            .resolved
            .tools
            .iter()
            .find(|tool| tool.name == "bash")
            .expect("the scoped context exposes the product bash tool");
        let output = (bash.execute)(
            ToolExecutionContext::standalone("bash"),
            serde_json::json!({"command": "pwd"}),
            None,
        )
        .await
        .unwrap();
        let output = output
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(output.trim(), canonical_a.to_string_lossy());

        let options_a = context_a.session_options().unwrap();
        let options_b = context_b.session_options().unwrap();
        assert_eq!(
            options_a.session_log_root(),
            Some(shared_sessions.as_path())
        );
        assert_eq!(
            options_b.session_log_root(),
            Some(shared_sessions.as_path())
        );

        let _session_a = context_a
            .create_session_with_id("workspace-project-a")
            .await
            .unwrap();
        let _session_b = context_b
            .session_bootstrap()
            .with_new_session()
            .open()
            .await
            .unwrap();
        let overviews_a = context_a.session_query().unwrap().overviews().unwrap();
        let overviews_b = context_b.session_query().unwrap().overviews().unwrap();
        assert_eq!(overviews_a.overviews.len(), 1);
        assert_eq!(overviews_b.overviews.len(), 1);
        assert_eq!(
            overviews_a.overviews[0].workspace.kind,
            CodingAgentWorkspaceKind::Project
        );
        assert_ne!(
            overviews_a.overviews[0].workspace.group_id,
            overviews_b.overviews[0].workspace.group_id
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projectless_context_ignores_managed_scratch_project_state() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR", "EVO_SESSION_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            "default_thinking_level = \"low\"\n",
        )
        .unwrap();
        std::fs::write(global.join("AGENTS.md"), "global context").unwrap();
        write_test_skill(&global, "global-skill");
        env.set_evo_dir(&global);

        let options = CodingAgentEmbeddingOptions::for_workspace(
            CodingAgentWorkspaceSelection::projectless("home-workspace"),
        )
        .unwrap();
        let workspace = options.workspace().unwrap().clone();
        std::fs::create_dir_all(workspace.execution_cwd.join(".evo")).unwrap();
        std::fs::write(
            workspace.execution_cwd.join(".evo/settings.toml"),
            "default_thinking_level = \"xhigh\"\n",
        )
        .unwrap();
        std::fs::write(
            workspace.execution_cwd.join("AGENTS.md"),
            "scratch project context must not load",
        )
        .unwrap();
        write_test_skill(
            &workspace.execution_cwd.join(".evo"),
            "scratch-project-skill",
        );

        let context = CodingAgentEmbeddingContext::load(options).unwrap();

        assert_eq!(context.snapshot().cwd, workspace.execution_cwd);
        assert_eq!(context.snapshot().workspace.as_ref(), Some(&workspace));
        assert_eq!(
            workspace.overview.kind,
            CodingAgentWorkspaceKind::Projectless
        );
        assert_eq!(workspace.overview.display_path, None);
        assert_eq!(
            context
                .snapshot()
                .settings
                .default_thinking_level
                .as_deref(),
            Some("low")
        );
        assert!(
            context
                .snapshot()
                .resources
                .skill_names
                .iter()
                .any(|name| name == "global-skill")
        );
        assert!(
            !context
                .snapshot()
                .resources
                .skill_names
                .iter()
                .any(|name| name == "scratch-project-skill")
        );
        assert_eq!(
            context.snapshot().resources.context_files,
            vec![global.join("AGENTS.md")]
        );

        let _session = context
            .create_session_with_id("workspace-projectless")
            .await
            .unwrap();
        let overviews = context.session_query().unwrap().overviews().unwrap();
        assert_eq!(overviews.overviews.len(), 1);
        assert_eq!(
            overviews.overviews[0].workspace.kind,
            CodingAgentWorkspaceKind::Projectless
        );
        assert_eq!(overviews.overviews[0].workspace.display_path, None);
    }

    #[test]
    fn anthropic_capability_exposes_budget_levels_and_explicit_disable() {
        let mut model = thinking_model("anthropic-messages", true);
        model.thinking_level_map = Some(ThinkingLevelMap {
            xhigh: Some(ThinkingLevelValue::String("max".into())),
            ..Default::default()
        });
        model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            ..Default::default()
        }));

        let entry = model_catalog_entry(&model);

        assert_eq!(
            entry.thinking_capability.explicit_levels,
            EXPLICIT_THINKING_LEVELS
        );
        assert!(entry.thinking_capability.supported);
        assert!(entry.thinking_capability.can_disable);
    }

    #[test]
    fn openai_capability_filters_explicit_null_mapping_and_cannot_disable() {
        let mut model = thinking_model("openai-responses", true);
        model.thinking_level_map = Some(ThinkingLevelMap {
            minimal: Some(ThinkingLevelValue::Null),
            xhigh: Some(ThinkingLevelValue::String("xhigh".into())),
            ..Default::default()
        });

        let entry = model_catalog_entry(&model);

        assert_eq!(
            entry.thinking_capability.explicit_levels,
            vec![
                CodingAgentThinkingLevel::Low,
                CodingAgentThinkingLevel::Medium,
                CodingAgentThinkingLevel::High,
                CodingAgentThinkingLevel::XHigh,
            ]
        );
        assert!(!entry.thinking_capability.can_disable);
        assert!(entry.thinking_capability.supported);
    }

    #[test]
    fn reasoning_api_without_a_level_map_uses_its_complete_default_matrix() {
        let model = thinking_model("openai-responses", true);

        assert_eq!(
            model_catalog_entry(&model)
                .thinking_capability
                .explicit_levels,
            EXPLICIT_THINKING_LEVELS
        );
    }

    #[test]
    fn non_reasoning_model_has_no_thinking_controls() {
        let model = thinking_model("anthropic-messages", false);

        assert_eq!(
            model_catalog_entry(&model).thinking_capability,
            CodingAgentThinkingCapability::default()
        );
    }

    #[test]
    fn reasoning_model_without_tunable_api_keeps_auto_only_capability() {
        let model = thinking_model("openai-completions", true);

        assert_eq!(
            model_catalog_entry(&model).thinking_capability,
            CodingAgentThinkingCapability {
                supported: true,
                explicit_levels: Vec::new(),
                can_disable: false,
            }
        );
    }

    #[test]
    fn openai_deepseek_compatibility_allows_explicit_disable() {
        let mut model = thinking_model("openai-completions", true);
        model.compat = Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            thinking_format: Some(ThinkingFormat::DeepSeek),
            ..Default::default()
        }));

        let capability = model_catalog_entry(&model).thinking_capability;

        assert_eq!(capability.explicit_levels, EXPLICIT_THINKING_LEVELS);
        assert!(capability.can_disable);
    }

    #[test]
    fn unsupported_explicit_thinking_request_falls_back_to_auto() {
        let model = thinking_model("openai-responses", true);
        let entry = model_catalog_entry(&model);
        let choice = CodingAgentModelChoice {
            id: entry.id,
            name: entry.name,
            provider: entry.provider,
            reasoning: entry.reasoning,
            thinking_capability: entry.thinking_capability,
            supports_text: entry.supports_text,
            supports_images: entry.supports_images,
            context_window: entry.context_window,
            max_output_tokens: entry.max_output_tokens,
            configured: true,
            selected: true,
        };

        assert_eq!(
            sanitize_thinking_level(&choice, CodingAgentThinkingLevel::High),
            CodingAgentThinkingLevelSanitization::Explicit(CodingAgentThinkingLevel::High)
        );
        assert_eq!(
            sanitize_thinking_level(&choice, CodingAgentThinkingLevel::Off),
            CodingAgentThinkingLevelSanitization::AutoFallback
        );
    }

    #[test]
    fn embedding_prepares_explicit_file_attachments_with_product_bounds() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        env.set_evo_dir(&global);
        let attachment = cwd.join("notes with spaces.txt");
        std::fs::write(&attachment, "bounded attachment body").unwrap();
        let context =
            CodingAgentEmbeddingContext::load(CodingAgentEmbeddingOptions::new(&cwd)).unwrap();

        let prepared = context
            .prepare_prompt_with_attachments("review this", std::slice::from_ref(&attachment))
            .unwrap();
        assert!(
            prepared
                .display_text()
                .starts_with("review this\n<file name=")
        );
        assert!(prepared.display_text().contains("bounded attachment body"));
        assert!(!format!("{prepared:?}").contains("bounded attachment body"));
        let operation = context.prepared_prompt_operation(prepared, None);
        let CodingAgentOperation::Prompt(options) = operation else {
            panic!("prepared prompt must construct a prompt operation");
        };
        assert!(matches!(
            options.invocation(),
            PromptInvocation::Text(text) if text.contains("bounded attachment body")
        ));

        let too_many = vec![attachment; crate::limits::MAX_AT_FILE_REFERENCES + 1];
        let error = context
            .prepare_prompt_with_attachments("review", &too_many)
            .unwrap_err();
        assert_eq!(error.category, CodingAgentErrorCategory::Input);
        assert_eq!(error.code(), "invalid_input");
    }

    #[test]
    fn global_only_embedding_ignores_project_configuration_and_resources() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let scratch = temp.path().join("scratch/workspace-1");
        std::fs::create_dir_all(global.join("skills/global-skill")).unwrap();
        std::fs::create_dir_all(scratch.join(".evo/skills/project-skill")).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            "default_thinking_level = \"low\"\n",
        )
        .unwrap();
        std::fs::write(
            scratch.join(".evo/settings.toml"),
            "default_thinking_level = \"high\"\n",
        )
        .unwrap();
        std::fs::write(
            global.join("skills/global-skill/SKILL.md"),
            "---\nname: global-skill\ndescription: global\n---\nglobal\n",
        )
        .unwrap();
        std::fs::write(
            scratch.join(".evo/skills/project-skill/SKILL.md"),
            "---\nname: project-skill\ndescription: project\n---\nproject\n",
        )
        .unwrap();
        std::fs::write(global.join("AGENTS.md"), "global context").unwrap();
        std::fs::write(scratch.join("AGENTS.md"), "scratch project context").unwrap();
        env.set_evo_dir(&global);

        let options = CodingAgentEmbeddingOptions::new(&scratch).with_global_config_only();
        assert!(options.global_config_only());
        let context = CodingAgentEmbeddingContext::load(options).unwrap();
        let snapshot = context.snapshot();

        assert_eq!(global_config_directory(), global);
        assert_eq!(snapshot.cwd, scratch);
        assert_eq!(
            snapshot.settings.default_thinking_level.as_deref(),
            Some("low")
        );
        assert!(
            snapshot
                .resources
                .skill_names
                .iter()
                .any(|name| name == "global-skill")
        );
        assert!(
            !snapshot
                .resources
                .skill_names
                .iter()
                .any(|name| name == "project-skill")
        );
        assert_eq!(
            snapshot.resources.context_files,
            vec![global.join("AGENTS.md")]
        );
    }

    #[test]
    fn cwd_free_catalogs_load_global_state_without_an_embedding_context() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let external = temp.path().join("external-skills");
        std::fs::create_dir_all(global.join("skills/global-skill")).unwrap();
        std::fs::create_dir_all(external.join("configured-skill")).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            format!(
                "default_provider = \"openai\"\ntheme = \"global-home\"\nskills = [{}]\n",
                toml::Value::String(external.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        std::fs::write(
            global.join("skills/global-skill/SKILL.md"),
            "---\nname: global-skill\ndescription: Globally visible\n---\nglobal-body-secret-canary\n",
        )
        .unwrap();
        std::fs::write(
            external.join("configured-skill/SKILL.md"),
            "---\nname: configured-skill\ndescription: Must stay excluded\n---\nexternal\n",
        )
        .unwrap();
        let mut auth = AuthStore::default();
        auth.set_api_key("anthropic", "catalog-auth-secret-canary");
        auth.set_api_key("openai", "second-catalog-auth-secret-canary");
        auth.save(&global.join("auth.toml")).unwrap();
        env.set_evo_dir(&global);

        let settings = global_settings_snapshot();
        let auth = global_auth_snapshot();
        let models = configured_model_catalog();
        let skills = global_skill_catalog();

        assert_eq!(settings.presentation.theme.as_deref(), Some("global-home"));
        assert!(auth.providers.iter().any(|provider| {
            provider.provider == "anthropic" && provider.kind == CodingAgentProviderAuthKind::ApiKey
        }));
        assert!(models.iter().any(|model| model.provider == "anthropic"));
        assert_eq!(
            models.first().map(|model| model.provider.as_str()),
            Some("openai")
        );
        assert_eq!(
            skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["global-skill"]
        );
        let public_debug = format!("{settings:?}{auth:?}{models:?}{skills:?}");
        assert!(!public_debug.contains("catalog-auth-secret-canary"));
        assert!(!public_debug.contains("second-catalog-auth-secret-canary"));
        assert!(!public_debug.contains("global-body-secret-canary"));
        assert!(!public_debug.contains(&external.to_string_lossy().into_owned()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caller_assigned_session_id_is_normalized_and_created_once() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let cwd = temp.path().join("project");
        let sessions = temp.path().join("sessions");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        env.set_evo_dir(&global);
        let context = CodingAgentEmbeddingContext::load(
            CodingAgentEmbeddingOptions::new(&cwd).with_session_dir(&sessions),
        )
        .unwrap();

        let session = context
            .create_session_with_id("  caller_session_01  ")
            .await
            .unwrap();

        assert_eq!(session.view().session_id, "caller_session_01");
        assert!(sessions.join("caller_session_01/session.json").is_file());
        assert!(sessions.join("caller_session_01/events.jsonl").is_file());

        let error = context
            .create_session_with_id("caller_session_01")
            .await
            .unwrap_err();
        assert_eq!(error.category, CodingAgentErrorCategory::Session);
        assert_eq!(error.code(), "session");
        assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caller_assigned_session_id_requires_persistence() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        env.set_evo_dir(&global);
        let context = CodingAgentEmbeddingContext::load(
            CodingAgentEmbeddingOptions::new(&cwd).with_session_mode(SessionMode::Disabled),
        )
        .unwrap();

        let error = context
            .create_session_with_id("disabled_session")
            .await
            .unwrap_err();

        assert_eq!(error.category, CodingAgentErrorCategory::Capability);
        assert_eq!(error.code(), "unsupported_capability");
        assert!(!global.join("sessions").exists());
    }
}

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

fn thinking_capability(model: &Model) -> CodingAgentThinkingCapability {
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

fn thinking_level_is_mapped(
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

fn api_supports_explicit_thinking(model: &Model) -> bool {
    match model.api.as_str() {
        "anthropic-messages"
        | "azure-openai-responses"
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

fn api_can_disable_thinking(model: &Model) -> bool {
    match model.api.as_str() {
        "anthropic-messages" | "mistral-conversations" => true,
        "openai-completions" => openai_completions_compat(model)
            .is_some_and(|compat| compat.thinking_format == Some(ThinkingFormat::DeepSeek)),
        _ => false,
    }
}

fn openai_completions_compat(
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

fn profile_choice_from_agent(profile: &AgentProfile) -> CodingAgentProfileChoice {
    CodingAgentProfileChoice {
        id: profile.id.clone(),
        display_name: safe_public_summary(&profile.display_name),
        description: profile.description.as_deref().map(safe_public_summary),
        kind: ProfileKind::Agent,
        source: profile.source,
        model_id: profile.model.clone(),
    }
}

fn profile_choice_from_team(profile: &TeamProfile) -> CodingAgentProfileChoice {
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

fn embedding_diagnostic_from_profile(
    diagnostic: &ProfileDiagnostic,
) -> CodingAgentPublicDiagnostic {
    CodingAgentPublicDiagnostic::from_profile_diagnostic(diagnostic)
}

fn default_thinking_level(resolved: &ResolvedApplicationContext) -> Option<ThinkingLevel> {
    resolved
        .config
        .settings
        .default_thinking_level
        .as_deref()
        .and_then(|level| level.parse().ok())
}
