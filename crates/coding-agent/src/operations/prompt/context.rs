use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent_core::api::agent::{Agent, AgentEvent, AgentResources, ProviderStreamer, ThinkingLevel};
use agent_core::api::tool::{AgentTool, AgentToolResult, ToolExecutionContext, ToolExecutionMode};
use ai::api::auth::ProviderAuthDiagnostic;
use ai::api::conversation::{AssistantMessage, ContentBlock};
use ai::api::model::Model;
use ai::api::stream::AssistantMessageEvent;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::{PromptRuntimeOptions, assistant_text};
use crate::app::session::ResolvedSessionTarget;
use crate::app::startup::ResolvedPromptRequest;
use crate::config::Settings;

use crate::events::prompt_stream::PromptStreamEvent;
use crate::operations::delegation::{
    DelegationAuthorizationDecision, DelegationLineageEntry, DelegationTargetInventory,
    PendingDelegationConfirmationState, authorize_delegation_requests_with_lineage,
};
use crate::profiles::{AgentProfile, DelegationPolicy, ProfileId, ProfileKind, ProfileRegistry};
use crate::runtime::capability::OperationCapabilitySnapshot;
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::control::PromptControlReceiver;
use crate::services::authorization::{AuthorizationHookContext, AuthorizationService};
use crate::services::event::{AgentEventMappingContext, EventService, map_agent_event};
use crate::session::event::{
    DiagnosticLevel, PersistedContentBlock, PersistedDelegationStatus, PersistedToolResult,
};
use crate::session::id::{SystemClock, SystemIdGenerator};
use crate::session::replay::{MessageStatus, SessionReplay, TranscriptItem};
use crate::session::service::SessionEventWriter;
use crate::session::transaction::TurnTransaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTurnMode {
    Print,
    Json,
    Rpc,
}

#[derive(Debug, Clone)]
pub struct PromptTurnOptions {
    invocation: PromptInvocation,
    mode: PromptTurnMode,
    session_target: Option<ResolvedSessionTarget>,
    session_name: Option<String>,
    runtime: Option<RuntimeSnapshot>,
    queued_steering: Vec<QueuedPromptInput>,
    queued_follow_up: Vec<QueuedPromptInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum QueuedPromptInput {
    Text(String),
    Content(Vec<ContentBlock>),
}

impl PromptTurnOptions {
    pub fn new(invocation: PromptInvocation) -> Self {
        Self {
            invocation,
            mode: PromptTurnMode::Print,
            session_target: None,
            session_name: None,
            runtime: None,
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
        }
    }

    pub fn with_mode(mut self, mode: PromptTurnMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_session_target(mut self, target: ResolvedSessionTarget) -> Self {
        self.session_target = Some(target);
        self
    }

    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
    }

    pub fn invocation(&self) -> &PromptInvocation {
        &self.invocation
    }

    pub fn mode(&self) -> PromptTurnMode {
        self.mode
    }

    pub fn session_target(&self) -> Option<&ResolvedSessionTarget> {
        self.session_target.as_ref()
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    pub(crate) fn with_queued_inputs(
        mut self,
        steering: Vec<QueuedPromptInput>,
        follow_up: Vec<QueuedPromptInput>,
    ) -> Self {
        self.queued_steering = steering;
        self.queued_follow_up = follow_up;
        self
    }

    pub(crate) fn queued_steering(&self) -> &[QueuedPromptInput] {
        &self.queued_steering
    }

    pub(crate) fn queued_follow_up(&self) -> &[QueuedPromptInput] {
        &self.queued_follow_up
    }

    pub(crate) fn from_prompt_runtime_options(options: PromptRuntimeOptions) -> Self {
        let invocation = options.invocation.clone();
        let session_target = options.session_target.clone();
        let session_name = options.session_name.clone();
        let runtime = RuntimeSnapshot::from_prompt_runtime_options(options);
        Self {
            invocation,
            mode: PromptTurnMode::Print,
            session_target,
            session_name,
            runtime: Some(runtime),
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
        }
    }

    pub(crate) fn runtime(&self) -> Option<&RuntimeSnapshot> {
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut RuntimeSnapshot> {
        self.runtime.as_mut()
    }

    pub(crate) fn set_invocation(&mut self, invocation: PromptInvocation) {
        self.invocation = invocation;
    }

    pub(crate) fn apply_agent_profile(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        diagnostics: Vec<CodingDiagnostic>,
    ) -> Result<(), CodingSessionError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn options do not include a runtime snapshot".into(),
            })?;
        runtime.apply_agent_profile(profile, registry, diagnostics);
        Ok(())
    }

    pub(crate) fn apply_delegated_agent_profile(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        diagnostics: Vec<CodingDiagnostic>,
    ) -> Result<(), CodingSessionError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn options do not include a runtime snapshot".into(),
            })?;
        runtime.apply_delegated_agent_profile(profile, registry, diagnostics);
        Ok(())
    }
}

impl From<&ResolvedPromptRequest> for PromptTurnOptions {
    fn from(request: &ResolvedPromptRequest) -> Self {
        Self {
            invocation: request.invocation.clone(),
            mode: request.context.invocation_options.prompt_mode,
            session_target: request.context.session_target.clone(),
            session_name: request.context.session_name.clone(),
            runtime: None,
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
        }
    }
}

impl From<ResolvedPromptRequest> for PromptTurnOptions {
    fn from(request: ResolvedPromptRequest) -> Self {
        let mut options = PromptTurnOptions::from(&request);
        options.runtime = Some(RuntimeSnapshot::from_prompt_runtime_options(
            request.session_options,
        ));
        options
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodingDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodingDiagnostic {
    pub severity: CodingDiagnosticSeverity,
    pub message: String,
    pub source: Option<std::path::PathBuf>,
    pub code: Option<String>,
}

impl CodingDiagnostic {
    pub(crate) fn info(message: impl Into<String>) -> Self {
        Self {
            severity: CodingDiagnosticSeverity::Info,
            message: message.into(),
            source: None,
            code: None,
        }
    }

    pub(crate) fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: CodingDiagnosticSeverity::Warning,
            message: message.into(),
            source: None,
            code: None,
        }
    }

    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self {
            severity: CodingDiagnosticSeverity::Error,
            message: message.into(),
            source: None,
            code: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "typed prompt outcome preserves the final provider message without a second allocation"
)]
pub(crate) enum InternalPromptTurnOutcome {
    Success {
        operation_id: String,
        turn_id: String,
        session_id: Option<String>,
        leaf_id: Option<String>,
        final_text: String,
        final_message: AssistantMessage,
        diagnostics: Vec<CodingDiagnostic>,
    },
    Aborted {
        operation_id: String,
        turn_id: Option<String>,
        reason: String,
        session_id: Option<String>,
    },
    Failed {
        operation_id: String,
        turn_id: Option<String>,
        error: CodingSessionError,
        diagnostics: Vec<CodingDiagnostic>,
    },
}

impl InternalPromptTurnOutcome {
    pub(crate) fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    pub(crate) fn partial_commit_error(&self) -> Option<&CodingSessionError> {
        match self {
            Self::Failed {
                error: error @ CodingSessionError::PartialCommit { .. },
                ..
            } => Some(error),
            Self::Success { .. } | Self::Aborted { .. } | Self::Failed { .. } => None,
        }
    }

    pub(crate) fn apply_success_session_write_metadata(
        &mut self,
        session_id: Option<String>,
        leaf_id: Option<String>,
    ) {
        let Self::Success {
            session_id: outcome_session_id,
            leaf_id: outcome_leaf_id,
            ..
        } = self
        else {
            return;
        };
        if let Some(session_id) = session_id {
            *outcome_session_id = Some(session_id);
        }
        *outcome_leaf_id = leaf_id;
    }
}

pub(crate) type PromptTurnTransaction = TurnTransaction<SystemIdGenerator, SystemClock>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegationRequest {
    pub(crate) operation_id: String,
    pub(crate) turn_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) requesting_profile_id: ProfileId,
    pub(crate) target_kind: ProfileKind,
    pub(crate) target_id: ProfileId,
    pub(crate) task: String,
}

#[derive(Clone)]
pub(crate) struct RuntimeSnapshot {
    model: Model,
    credential_provider: String,
    api_key: Option<String>,
    auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    system_prompt: Option<String>,
    max_turns: Option<u32>,
    tools: Vec<AgentTool>,
    register_builtins: bool,
    resources: AgentResources,
    settings: Option<Settings>,
    thinking_level: Option<ThinkingLevel>,
    tool_execution: Option<ToolExecutionMode>,
    session_run_options: Option<SessionRunOptions>,
    profile_id: Option<ProfileId>,
    profile_delegation_policy: Option<DelegationPolicy>,
    delegation_target_inventory: DelegationTargetInventory,
    profile_tool_allowlist: Option<Vec<String>>,
    profile_skill_allowlist: Option<Vec<String>>,
    profile_diagnostics: Vec<CodingDiagnostic>,
    provider_streamer: Option<ProviderStreamer>,
}

impl std::fmt::Debug for RuntimeSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSnapshot")
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("auth_diagnostics", &self.auth_diagnostics)
            .field("system_prompt", &self.system_prompt)
            .field("max_turns", &self.max_turns)
            .field("tools_len", &self.tools.len())
            .field("register_builtins", &self.register_builtins)
            .field("resources", &self.resources)
            .field("settings", &self.settings)
            .field("thinking_level", &self.thinking_level)
            .field("tool_execution", &self.tool_execution)
            .field("session_run_options", &self.session_run_options)
            .field("profile_id", &self.profile_id)
            .field("profile_delegation_policy", &self.profile_delegation_policy)
            .field(
                "delegation_target_inventory",
                &self.delegation_target_inventory,
            )
            .field("profile_tool_allowlist", &self.profile_tool_allowlist)
            .field("profile_skill_allowlist", &self.profile_skill_allowlist)
            .field("profile_diagnostics", &self.profile_diagnostics)
            .field("has_provider_streamer", &self.provider_streamer.is_some())
            .finish()
    }
}

impl RuntimeSnapshot {
    pub(crate) fn from_prompt_runtime_options(options: PromptRuntimeOptions) -> Self {
        let PromptRuntimeOptions {
            model,
            api_key,
            auth_diagnostics,
            system_prompt,
            max_turns,
            tools,
            register_builtins,
            ai_client,
            session,
            session_target: _,
            session_name: _,
            thinking_level,
            tool_execution,
            resources,
            settings,
            invocation: _,
        } = options;
        let credential_provider = model.provider.clone();

        Self {
            model,
            credential_provider,
            api_key,
            auth_diagnostics,
            system_prompt,
            max_turns,
            tools,
            register_builtins,
            resources,
            settings,
            thinking_level,
            tool_execution,
            session_run_options: session,
            profile_id: None,
            profile_delegation_policy: None,
            delegation_target_inventory: DelegationTargetInventory::default(),
            profile_tool_allowlist: None,
            profile_skill_allowlist: None,
            profile_diagnostics: Vec::new(),
            provider_streamer: ai_client.map(|ai_client| {
                if register_builtins {
                    ai_client.register_builtins();
                }
                let ai_client = std::sync::Arc::new(ai_client);
                let provider_streamer: ProviderStreamer =
                    std::sync::Arc::new(move |model, context, options| {
                        ai_client.stream_model(model, context, options)
                    });
                provider_streamer
            }),
        }
    }

    pub(crate) fn apply_agent_profile(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        diagnostics: Vec<CodingDiagnostic>,
    ) {
        self.apply_agent_profile_core(profile, registry, diagnostics);
        self.profile_tool_allowlist = (!profile.tools.is_empty()).then(|| profile.tools.clone());
        self.profile_skill_allowlist = (!profile.skills.is_empty()).then(|| profile.skills.clone());
    }

    pub(crate) fn apply_delegated_agent_profile(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        diagnostics: Vec<CodingDiagnostic>,
    ) {
        self.apply_agent_profile_core(profile, registry, diagnostics);
        self.profile_tool_allowlist = Some(profile.tools.clone());
        self.profile_skill_allowlist = Some(profile.skills.clone());
    }

    fn apply_agent_profile_core(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        mut diagnostics: Vec<CodingDiagnostic>,
    ) {
        self.profile_diagnostics.append(&mut diagnostics);
        if let Some(model_id) = profile.model.as_deref() {
            match ai::api::model::lookup_model(model_id) {
                Some(model) => self.model = model,
                None => self
                    .profile_diagnostics
                    .push(CodingDiagnostic::warning(format!(
                        "agent profile {} requested unavailable model: {model_id}",
                        profile.id
                    ))),
            }
        }
        if let Some(system_prompt) = profile.system_prompt.as_ref() {
            self.system_prompt = Some(system_prompt.clone());
        }
        self.profile_id = Some(profile.id.clone());
        self.profile_delegation_policy = Some(profile.delegation.clone());
        self.delegation_target_inventory =
            DelegationTargetInventory::from_registry(registry, &profile.delegation);
    }

    pub(crate) fn model(&self) -> &Model {
        &self.model
    }

    pub(crate) fn with_model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    pub(crate) fn api_key(&self) -> Option<&str> {
        (self.model.provider == self.credential_provider)
            .then_some(self.api_key.as_deref())
            .flatten()
    }

    pub(crate) fn auth_diagnostics(&self) -> &[ProviderAuthDiagnostic] {
        if self.model.provider == self.credential_provider {
            &self.auth_diagnostics
        } else {
            &[]
        }
    }

    pub(crate) fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub(crate) fn max_turns(&self) -> Option<u32> {
        self.max_turns
    }

    pub(crate) fn tools(&self) -> &[AgentTool] {
        &self.tools
    }

    pub(crate) fn register_builtins(&self) -> bool {
        self.register_builtins
    }

    pub(crate) fn resources(&self) -> &AgentResources {
        &self.resources
    }

    pub(crate) fn settings(&self) -> Option<&Settings> {
        self.settings.as_ref()
    }

    pub(crate) fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.thinking_level
    }

    pub(crate) fn tool_execution(&self) -> Option<ToolExecutionMode> {
        self.tool_execution
    }

    pub(crate) fn session_run_options(&self) -> Option<&SessionRunOptions> {
        self.session_run_options.as_ref()
    }

    pub(crate) fn cwd(&self) -> Option<&std::path::Path> {
        self.session_run_options
            .as_ref()
            .map(|options| options.cwd.as_path())
    }

    pub(crate) fn profile_id(&self) -> Option<&ProfileId> {
        self.profile_id.as_ref()
    }

    pub(crate) fn profile_delegation_policy(&self) -> Option<&DelegationPolicy> {
        self.profile_delegation_policy.as_ref()
    }

    pub(crate) fn delegation_target_inventory(&self) -> &DelegationTargetInventory {
        &self.delegation_target_inventory
    }

    pub(crate) fn profile_tool_allowlist(&self) -> Option<&[String]> {
        self.profile_tool_allowlist.as_deref()
    }

    pub(crate) fn profile_skill_allowlist(&self) -> Option<&[String]> {
        self.profile_skill_allowlist.as_deref()
    }

    pub(crate) fn profile_diagnostics(&self) -> &[CodingDiagnostic] {
        &self.profile_diagnostics
    }

    pub(crate) fn provider_streamer(&self) -> Option<&ProviderStreamer> {
        self.provider_streamer.as_ref()
    }

    pub(crate) fn set_provider_streamer(&mut self, provider_streamer: ProviderStreamer) {
        self.provider_streamer = Some(provider_streamer);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptTurnIds {
    operation_id: String,
    turn_id: String,
}

impl PromptTurnIds {
    pub(crate) fn new(operation_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
        }
    }
}

pub(crate) struct PromptTurnContext {
    ids: PromptTurnIds,
    options: PromptTurnOptions,
    request_resolved: bool,
    runtime: Option<RuntimeSnapshot>,
    prepared_input: Option<Vec<PersistedContentBlock>>,
    loaded_resources: Option<AgentResources>,
    replay: Option<SessionReplay>,
    session_id: Option<String>,
    non_persistent_runtime_id: Option<String>,
    agent: Option<Agent>,
    transaction: Option<PromptTurnTransaction>,
    final_message: Option<AssistantMessage>,
    completion_recorded: bool,
    coding_events: Vec<PromptStreamEvent>,
    delegation_requests: Vec<DelegationRequest>,
    delegation_authorization_decisions: Vec<DelegationAuthorizationDecision>,
    assistant_session_message_id: Option<String>,
    completed_assistant_session_message_id: Option<String>,
    reasoning_duration: ReasoningDurationTracker,
    live_event_service: Option<EventService>,
    prompt_control_receiver: Option<PromptControlReceiver>,
    operation_cancellation: Option<CancellationToken>,
    authorization_service: Option<AuthorizationService>,
    authorization_event_writer: Option<SessionEventWriter>,
    tool_session_call_ids: HashMap<String, String>,
    diagnostics: Vec<CodingDiagnostic>,
    requested_abort_reason: Option<String>,
    capability_snapshot: Option<OperationCapabilitySnapshot>,
    delegation_executor: Option<DelegationToolExecutor>,
    deferred_pending_delegations: Arc<Mutex<Vec<PendingDelegationConfirmationState>>>,
}

pub(crate) type DelegationToolExecutor = Arc<
    dyn Fn(
            ToolExecutionContext,
            serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

impl PromptTurnContext {
    pub(crate) fn new(ids: PromptTurnIds, options: PromptTurnOptions) -> Self {
        Self {
            ids,
            options,
            request_resolved: false,
            runtime: None,
            prepared_input: None,
            loaded_resources: None,
            replay: None,
            session_id: None,
            non_persistent_runtime_id: None,
            agent: None,
            transaction: None,
            final_message: None,
            completion_recorded: false,
            coding_events: Vec::new(),
            delegation_requests: Vec::new(),
            delegation_authorization_decisions: Vec::new(),
            assistant_session_message_id: None,
            completed_assistant_session_message_id: None,
            reasoning_duration: ReasoningDurationTracker::default(),
            live_event_service: None,
            prompt_control_receiver: None,
            operation_cancellation: None,
            authorization_service: None,
            authorization_event_writer: None,
            tool_session_call_ids: HashMap::new(),
            diagnostics: Vec::new(),
            requested_abort_reason: None,
            capability_snapshot: None,
            delegation_executor: None,
            deferred_pending_delegations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.ids.operation_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.ids.turn_id
    }

    pub(crate) fn options(&self) -> &PromptTurnOptions {
        &self.options
    }

    pub(crate) fn set_authorization_service(&mut self, service: AuthorizationService) {
        self.authorization_service = Some(service);
    }

    pub(crate) fn set_authorization_event_writer(&mut self, writer: SessionEventWriter) {
        self.authorization_event_writer = Some(writer);
    }

    pub(crate) fn authorization_hook_context(&self) -> Option<AuthorizationHookContext> {
        let service = self.authorization_service.as_ref()?;
        let capability_snapshot = self.capability_snapshot.as_ref()?;
        Some(AuthorizationHookContext {
            service: service.clone(),
            turn_id: self.turn_id().to_owned(),
            capability_snapshot: capability_snapshot.clone(),
            event_writer: self.authorization_event_writer.clone(),
        })
    }

    pub(crate) fn set_capability_snapshot(&mut self, snapshot: OperationCapabilitySnapshot) {
        self.capability_snapshot = Some(snapshot);
    }

    pub(crate) fn set_delegation_executor(&mut self, executor: DelegationToolExecutor) {
        self.delegation_executor = Some(executor);
    }

    pub(crate) fn delegation_executor(&self) -> Option<DelegationToolExecutor> {
        self.delegation_executor.clone()
    }

    pub(crate) fn has_delegation_executor(&self) -> bool {
        self.delegation_executor.is_some()
    }

    pub(crate) fn deferred_pending_delegations(
        &self,
    ) -> Arc<Mutex<Vec<PendingDelegationConfirmationState>>> {
        self.deferred_pending_delegations.clone()
    }

    pub(crate) fn take_deferred_pending_delegations(
        &self,
    ) -> Vec<PendingDelegationConfirmationState> {
        self.deferred_pending_delegations
            .lock()
            .expect("deferred delegation queue lock poisoned")
            .drain(..)
            .collect()
    }

    pub(crate) fn capability_snapshot(&self) -> Option<&OperationCapabilitySnapshot> {
        self.capability_snapshot.as_ref()
    }

    pub(crate) fn set_runtime(&mut self, runtime: RuntimeSnapshot) {
        self.runtime = Some(runtime);
    }

    pub(crate) fn resolve_request(&mut self) -> Result<(), CodingSessionError> {
        if self.request_resolved {
            return Ok(());
        }
        match self.options.invocation() {
            PromptInvocation::Text(text) if text.is_empty() => {
                return Err(CodingSessionError::Input {
                    message: "prompt turn requires non-empty text input".into(),
                });
            }
            PromptInvocation::Content(content) if content.is_empty() => {
                return Err(CodingSessionError::Input {
                    message: "prompt turn requires non-empty content input".into(),
                });
            }
            PromptInvocation::Compact { .. } => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: "manual compaction in PromptTurnRunner".into(),
                });
            }
            PromptInvocation::Text(_)
            | PromptInvocation::Content(_)
            | PromptInvocation::Skill { .. }
            | PromptInvocation::PromptTemplate { .. } => {}
        }
        if self.options.runtime().is_none() {
            return Err(CodingSessionError::Config {
                message: "prompt turn options do not include a runtime snapshot".into(),
            });
        }
        self.request_resolved = true;
        Ok(())
    }

    pub(crate) fn resolve_runtime_from_options(&mut self) -> Result<(), CodingSessionError> {
        if self.runtime.is_some() {
            return Ok(());
        }
        self.require_resolved_request("resolve runtime")?;
        let runtime =
            self.options
                .runtime()
                .cloned()
                .ok_or_else(|| CodingSessionError::Config {
                    message: "prompt turn options do not include a runtime snapshot".into(),
                })?;
        self.set_runtime(runtime);
        Ok(())
    }

    pub(crate) fn runtime(&self) -> Option<&RuntimeSnapshot> {
        self.runtime.as_ref()
    }

    pub(crate) fn prepare_input(&mut self) -> Result<(), CodingSessionError> {
        if self.prepared_input.is_some() {
            return Ok(());
        }
        self.require_resolved_request("prepare input")?;
        self.prepared_input = Some(persisted_content_blocks_from_invocation(
            self.options.invocation(),
        )?);
        Ok(())
    }

    pub(crate) fn load_resources_from_runtime(&mut self) -> Result<(), CodingSessionError> {
        if self.loaded_resources.is_some() {
            return Ok(());
        }
        let resources = self
            .runtime
            .as_ref()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn cannot load resources without a runtime snapshot".into(),
            })?
            .resources()
            .clone();
        self.loaded_resources = Some(resources);
        Ok(())
    }

    pub(crate) fn loaded_resources(&self) -> Option<&AgentResources> {
        self.loaded_resources.as_ref()
    }

    pub(crate) fn set_replay(&mut self, replay: SessionReplay) {
        self.replay = Some(replay);
    }

    pub(crate) fn replay(&self) -> Option<&SessionReplay> {
        self.replay.as_ref()
    }

    pub(crate) fn set_non_persistent_session(
        &mut self,
        runtime_id: impl Into<String>,
        transcript: Vec<TranscriptItem>,
    ) {
        let runtime_id = runtime_id.into();
        self.non_persistent_runtime_id = Some(runtime_id.clone());
        self.session_id = None;
        self.transaction = None;
        self.replay = Some(SessionReplay {
            session_id: runtime_id,
            committed_through_session_sequence: 0,
            cwd: None,
            active_leaf_id: None,
            leaves: Vec::new(),
            tree_labels: Default::default(),
            transcript,
            diagnostics: Vec::new(),
            pending_delegation_confirmations: Vec::new(),
            pending_tool_authorizations: Vec::new(),
            usage: Default::default(),
            operation_statuses: Default::default(),
        });
    }

    pub(crate) fn non_persistent_runtime_id(&self) -> Option<&str> {
        self.non_persistent_runtime_id.as_deref()
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub(crate) fn set_session_id(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
        self.non_persistent_runtime_id = None;
    }

    pub(crate) fn set_agent(&mut self, agent: Agent) {
        self.agent = Some(agent);
    }

    pub(crate) fn agent(&self) -> Option<&Agent> {
        self.agent.as_ref()
    }

    pub(crate) fn set_transaction(&mut self, transaction: PromptTurnTransaction) {
        self.transaction = Some(transaction);
    }

    pub(crate) fn has_active_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    pub(crate) fn take_transaction(&mut self) -> Option<PromptTurnTransaction> {
        self.transaction.take()
    }

    pub(crate) fn enable_live_events(&mut self, event_service: EventService) {
        self.live_event_service = Some(event_service);
    }

    pub(crate) fn live_events_enabled(&self) -> bool {
        self.live_event_service.is_some()
    }

    pub(crate) fn set_prompt_control_receiver(&mut self, receiver: PromptControlReceiver) {
        self.prompt_control_receiver = Some(receiver);
    }

    pub(crate) fn take_prompt_control_receiver(&mut self) -> Option<PromptControlReceiver> {
        self.prompt_control_receiver.take()
    }

    pub(crate) fn set_operation_cancellation(&mut self, cancellation: CancellationToken) {
        self.operation_cancellation = Some(cancellation);
    }

    pub(crate) fn operation_cancellation(&self) -> Option<CancellationToken> {
        self.operation_cancellation.clone()
    }

    pub(crate) fn completed_transcript_items(&self) -> Vec<TranscriptItem> {
        let mut transcript = Vec::new();

        if let Some(input) = self.prepared_input.as_deref() {
            let text = persisted_content_blocks_text(input);
            if !text.is_empty() {
                transcript.push(TranscriptItem::UserInput {
                    turn_id: self.turn_id().to_owned(),
                    text,
                    started_at: None,
                });
            }
        }

        if let Some(message) = self.final_message.as_ref() {
            let content = persisted_assistant_content_blocks(&message.content);
            if !content.is_empty() {
                transcript.push(TranscriptItem::AssistantMessage {
                    message_id: self
                        .assistant_session_message_id
                        .clone()
                        .unwrap_or_else(|| format!("msg_{}", self.turn_id())),
                    content,
                    status: MessageStatus::Completed,
                    reasoning_duration_millis: None,
                    model_id: self
                        .final_message
                        .as_ref()
                        .and_then(|message| {
                            message
                                .response_model
                                .as_deref()
                                .or(Some(message.model.as_str()))
                        })
                        .map(str::to_owned),
                    completed_at: None,
                });
            }
        }

        transcript
    }

    pub(crate) fn record_user_input(&mut self) -> Result<(), CodingSessionError> {
        let content = self
            .prepared_input
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "prompt turn input has not been prepared".into(),
            })?;
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.record_user_input(content)?;
            transaction.checkpoint()?;
        }
        Ok(())
    }

    pub(crate) fn record_diagnostic(&mut self, diagnostic: CodingDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub(crate) fn record_delegation_folded_update(
        &mut self,
        request: &DelegationRequest,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Result<(), CodingSessionError> {
        if let Some(transaction) = self.transaction.as_mut() {
            let session_tool_call_id = self
                .tool_session_call_ids
                .get(&request.tool_call_id)
                .cloned()
                .unwrap_or_else(|| request.tool_call_id.clone());
            transaction.record_delegation_folded_update(
                session_tool_call_id,
                request.requesting_profile_id.clone(),
                request.target_kind,
                request.target_id.clone(),
                request.task.clone(),
                status,
                child_operation_id,
                summary,
            )?;
        }
        Ok(())
    }

    pub(crate) fn request_abort(&mut self, reason: impl Into<String>) {
        self.requested_abort_reason = Some(reason.into());
    }

    pub(crate) fn abort_reason(&self) -> Option<&str> {
        self.requested_abort_reason.as_deref()
    }

    pub(crate) fn record_final_message(&mut self, message: AssistantMessage) {
        self.final_message = Some(message);
    }

    pub(crate) fn final_message(&self) -> Option<&AssistantMessage> {
        self.final_message.as_ref()
    }

    pub(crate) fn record_agent_event(
        &mut self,
        event: AgentEvent,
    ) -> Result<Vec<PromptStreamEvent>, CodingSessionError> {
        self.record_agent_event_to_transaction(&event)?;
        self.reasoning_duration.observe(&event);
        let reasoning_duration_millis = if matches!(
            event,
            AgentEvent::LlmEvent(AssistantMessageEvent::Done { .. })
        ) {
            self.reasoning_duration.take_duration_millis()
        } else {
            None
        };
        let mut mapping_context = AgentEventMappingContext::new(
            self.operation_id().to_owned(),
            self.turn_id().to_owned(),
        );
        if let Some(message_id) = self
            .assistant_session_message_id
            .clone()
            .or_else(|| self.completed_assistant_session_message_id.clone())
        {
            mapping_context = mapping_context.with_assistant_message_id(message_id);
        }
        if reasoning_duration_millis.is_some() {
            mapping_context =
                mapping_context.with_reasoning_duration_millis(reasoning_duration_millis);
        }
        let coding_events = map_agent_event(&mapping_context, &event);
        self.record_delegation_requests(&coding_events);
        self.coding_events.extend(coding_events.clone());
        if let Some(event_service) = &self.live_event_service {
            for event in &coding_events {
                event_service.publish_prompt_stream_event(event.clone());
            }
        }
        Ok(coding_events)
    }

    pub(crate) fn coding_events(&self) -> &[PromptStreamEvent] {
        &self.coding_events
    }

    pub(crate) fn authorize_delegation_requests(
        &mut self,
        current_depth: usize,
    ) -> Result<&[DelegationAuthorizationDecision], CodingSessionError> {
        self.authorize_delegation_requests_with_lineage(current_depth, &[])
    }

    pub(crate) fn authorize_delegation_requests_with_lineage(
        &mut self,
        current_depth: usize,
        lineage: &[DelegationLineageEntry],
    ) -> Result<&[DelegationAuthorizationDecision], CodingSessionError> {
        if self.delegation_requests.is_empty() {
            self.delegation_authorization_decisions.clear();
            return Ok(&self.delegation_authorization_decisions);
        }
        let policy = self
            .runtime
            .as_ref()
            .and_then(RuntimeSnapshot::profile_delegation_policy)
            .cloned()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn cannot authorize delegation without active profile policy"
                    .into(),
            })?;
        self.delegation_authorization_decisions = authorize_delegation_requests_with_lineage(
            &self.delegation_requests,
            &policy,
            current_depth,
            lineage,
        );
        Ok(&self.delegation_authorization_decisions)
    }

    fn record_delegation_requests(&mut self, events: &[PromptStreamEvent]) {
        for event in events {
            if let PromptStreamEvent::Delegation(event) = event
                && event.is_requested()
            {
                let context = event.context();
                self.delegation_requests.push(DelegationRequest {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: context.tool_call_id.clone(),
                    requesting_profile_id: context.requesting_profile_id.clone(),
                    target_kind: context.target_kind,
                    target_id: context.target_id.clone(),
                    task: context.task.clone(),
                });
            }
        }
    }

    pub(crate) fn record_prompt_completed(&mut self) -> Result<(), CodingSessionError> {
        if self.final_message.is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot emit completion without a final assistant message"
                    .into(),
            });
        }

        if self.completion_recorded {
            return Ok(());
        }

        self.completion_recorded = true;
        Ok(())
    }

    fn record_agent_event_to_transaction(
        &mut self,
        event: &AgentEvent,
    ) -> Result<(), CodingSessionError> {
        if self.transaction.is_none() {
            return Ok(());
        }

        match event {
            AgentEvent::LlmEvent(event) => self.record_assistant_event_to_transaction(event),
            AgentEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                arguments,
                ..
            } => {
                self.ensure_tool_session_call_started(tool_call_id, tool_name, Some(arguments))?;
                Ok(())
            }
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                update,
            } => {
                let session_tool_call_id =
                    self.ensure_tool_session_call_started(tool_call_id, tool_name, None)?;
                let message = content_blocks_text(&update.content);
                self.transaction_mut_required()?
                    .record_tool_updated(session_tool_call_id, message)
            }
            AgentEvent::ToolCallEnd {
                tool_call_id,
                tool_name,
                result,
            } => self.record_tool_result_to_transaction(tool_call_id, tool_name, result),
            AgentEvent::AgentDone { .. } => Ok(()),
            AgentEvent::AgentError { error } => self
                .transaction_mut_required()?
                .emit_diagnostic(DiagnosticLevel::Error, error.clone()),
            AgentEvent::TurnStart { .. }
            | AgentEvent::BeforeProviderRequest { .. }
            | AgentEvent::SessionCompacted { .. } => Ok(()),
        }
    }

    fn record_assistant_event_to_transaction(
        &mut self,
        event: &AssistantMessageEvent,
    ) -> Result<(), CodingSessionError> {
        match event {
            AssistantMessageEvent::Start { .. }
            | AssistantMessageEvent::TextStart { .. }
            | AssistantMessageEvent::TextDelta { .. }
            | AssistantMessageEvent::ThinkingDelta { .. }
            | AssistantMessageEvent::ToolcallStart { .. }
            | AssistantMessageEvent::ToolcallDelta { .. }
            | AssistantMessageEvent::ToolcallEnd { .. }
            | AssistantMessageEvent::ProviderItemStart { .. }
            | AssistantMessageEvent::ProviderItemDelta { .. }
            | AssistantMessageEvent::ProviderItemEnd { .. } => {
                self.ensure_assistant_session_message_started()?;
                Ok(())
            }
            AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                let message_id = self.ensure_assistant_session_message_started()?;
                self.transaction_mut_required()?
                    .start_assistant_reasoning(message_id, *content_index)
            }
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                let message_id = self.assistant_session_message_id.clone().ok_or_else(|| {
                    CodingSessionError::Session {
                        message: "assistant reasoning ended before its message started".into(),
                    }
                })?;
                self.transaction_mut_required()?
                    .complete_assistant_reasoning(message_id, *content_index)
            }
            AssistantMessageEvent::Done { message, .. } => {
                self.complete_current_assistant_message(message)
            }
            AssistantMessageEvent::Error { message, .. } => {
                self.transaction_mut_required()?.emit_diagnostic(
                    DiagnosticLevel::Error,
                    message
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "assistant stream failed".into()),
                )
            }
            AssistantMessageEvent::TextEnd { .. } => Ok(()),
        }
    }

    fn record_tool_result_to_transaction(
        &mut self,
        agent_tool_call_id: &str,
        tool_name: &str,
        result: &AgentToolResult,
    ) -> Result<(), CodingSessionError> {
        let session_tool_call_id =
            self.ensure_tool_session_call_started(agent_tool_call_id, tool_name, None)?;
        let delegation_update = terminal_delegation_update(tool_name, &result.content);
        if result.is_error {
            self.transaction_mut_required()?.record_tool_failed(
                session_tool_call_id.clone(),
                content_blocks_text(&result.content),
            )?;
            if let Some(update) = delegation_update {
                self.transaction_mut_required()?
                    .record_delegation_folded_update(
                        session_tool_call_id,
                        update.requesting_profile_id,
                        update.target_kind,
                        update.target_id,
                        update.task,
                        update.status,
                        update.child_operation_id,
                        update.summary,
                    )?;
            }
            Ok(())
        } else {
            self.transaction_mut_required()?.record_tool_completed(
                session_tool_call_id.clone(),
                persisted_tool_result(&result.content),
            )?;
            if let Some(update) = delegation_update {
                self.transaction_mut_required()?
                    .record_delegation_folded_update(
                        session_tool_call_id,
                        update.requesting_profile_id,
                        update.target_kind,
                        update.target_id,
                        update.task,
                        update.status,
                        update.child_operation_id,
                        update.summary,
                    )?;
            }
            Ok(())
        }
    }

    fn ensure_assistant_session_message_started(&mut self) -> Result<String, CodingSessionError> {
        if let Some(message_id) = &self.assistant_session_message_id {
            return Ok(message_id.clone());
        }
        let message_id = self.transaction_mut_required()?.start_assistant_message()?;
        self.assistant_session_message_id = Some(message_id.clone());
        self.completed_assistant_session_message_id = None;
        Ok(message_id)
    }

    fn complete_current_assistant_message(
        &mut self,
        message: &AssistantMessage,
    ) -> Result<(), CodingSessionError> {
        let message_id = self.ensure_assistant_session_message_started()?;
        let content = persisted_assistant_content_blocks(&message.content);
        let model_id = message
            .response_model
            .as_deref()
            .unwrap_or(&message.model)
            .to_owned();
        self.transaction_mut_required()?
            .complete_assistant_message(
                message_id.clone(),
                content,
                stop_reason_string(message),
                message.usage.clone(),
                Some(model_id),
            )?;
        self.assistant_session_message_id = None;
        self.completed_assistant_session_message_id = Some(message_id);
        Ok(())
    }

    fn ensure_tool_session_call_started(
        &mut self,
        agent_tool_call_id: &str,
        tool_name: &str,
        arguments: Option<&serde_json::Value>,
    ) -> Result<String, CodingSessionError> {
        if let Some(tool_call_id) = self.tool_session_call_ids.get(agent_tool_call_id) {
            return Ok(tool_call_id.clone());
        }
        let arguments = arguments.cloned().unwrap_or_else(|| serde_json::json!({}));
        let tool_call_id = self
            .transaction_mut_required()?
            .record_tool_started(tool_name, arguments)?;
        self.tool_session_call_ids
            .insert(agent_tool_call_id.to_owned(), tool_call_id.clone());
        Ok(tool_call_id)
    }

    fn transaction_mut_required(
        &mut self,
    ) -> Result<&mut PromptTurnTransaction, CodingSessionError> {
        self.transaction
            .as_mut()
            .ok_or_else(|| CodingSessionError::Session {
                message: "prompt turn has no active transaction".into(),
            })
    }

    fn require_resolved_request(&self, action: &str) -> Result<(), CodingSessionError> {
        if self.request_resolved {
            return Ok(());
        }
        Err(CodingSessionError::Session {
            message: format!("prompt turn cannot {action} before request is resolved"),
        })
    }

    pub(crate) fn finish_success(
        &self,
        session_id: Option<String>,
        leaf_id: Option<String>,
    ) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
        let final_message =
            self.final_message
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "prompt turn cannot finish successfully without a final message"
                        .into(),
                })?;
        Ok(InternalPromptTurnOutcome::Success {
            operation_id: self.operation_id().to_owned(),
            turn_id: self.turn_id().to_owned(),
            session_id,
            leaf_id,
            final_text: assistant_text(&final_message),
            final_message,
            diagnostics: self.diagnostics.clone(),
        })
    }

    pub(crate) fn finish_abort(
        &self,
        reason: impl Into<String>,
        session_id: Option<String>,
    ) -> InternalPromptTurnOutcome {
        InternalPromptTurnOutcome::Aborted {
            operation_id: self.operation_id().to_owned(),
            turn_id: Some(self.turn_id().to_owned()),
            reason: reason.into(),
            session_id,
        }
    }

    pub(crate) fn finish_failure(&self, error: CodingSessionError) -> InternalPromptTurnOutcome {
        InternalPromptTurnOutcome::Failed {
            operation_id: self.operation_id().to_owned(),
            turn_id: Some(self.turn_id().to_owned()),
            error,
            diagnostics: self.diagnostics.clone(),
        }
    }
}

#[derive(Default)]
struct ReasoningDurationTracker {
    open: HashMap<u32, Instant>,
    completed_millis: u64,
    observed: bool,
}

impl ReasoningDurationTracker {
    fn observe(&mut self, event: &AgentEvent) {
        let AgentEvent::LlmEvent(event) = event else {
            return;
        };
        let now = Instant::now();
        match event {
            AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                self.start_at(*content_index, now);
            }
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                self.complete_at(*content_index, now);
            }
            AssistantMessageEvent::Done { .. } => self.finish_at(now),
            AssistantMessageEvent::Error { .. } => *self = Self::default(),
            _ => {}
        }
    }

    fn start_at(&mut self, content_index: u32, now: Instant) {
        self.observed = true;
        self.open.entry(content_index).or_insert(now);
    }

    fn complete_at(&mut self, content_index: u32, now: Instant) {
        let Some(started_at) = self.open.remove(&content_index) else {
            return;
        };
        self.completed_millis = self
            .completed_millis
            .saturating_add(duration_millis(started_at, now));
    }

    fn finish_at(&mut self, now: Instant) {
        let open = std::mem::take(&mut self.open);
        for started_at in open.into_values() {
            self.completed_millis = self
                .completed_millis
                .saturating_add(duration_millis(started_at, now));
        }
    }

    fn duration_millis(&self) -> Option<u64> {
        self.observed.then_some(self.completed_millis)
    }

    fn take_duration_millis(&mut self) -> Option<u64> {
        let duration = self.duration_millis();
        *self = Self::default();
        duration
    }
}

fn duration_millis(started_at: Instant, completed_at: Instant) -> u64 {
    u64::try_from(
        completed_at
            .saturating_duration_since(started_at)
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn stop_reason_string(message: &AssistantMessage) -> Option<String> {
    serde_json::to_value(&message.stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn persisted_tool_result(content: &[ContentBlock]) -> PersistedToolResult {
    PersistedToolResult::Text {
        text: content_blocks_text(content),
    }
}

struct TerminalDelegationUpdate {
    requesting_profile_id: ProfileId,
    target_kind: ProfileKind,
    target_id: ProfileId,
    task: String,
    status: PersistedDelegationStatus,
    child_operation_id: Option<String>,
    summary: Option<String>,
}

fn terminal_delegation_update(
    tool_name: &str,
    content: &[ContentBlock],
) -> Option<TerminalDelegationUpdate> {
    if !matches!(tool_name, "delegate_agent" | "delegate_team") {
        return None;
    }
    let text = content.iter().find_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let status = match value.get("status")?.as_str()? {
        "completed" => PersistedDelegationStatus::Completed,
        "failed" => PersistedDelegationStatus::Failed,
        "rejected" => PersistedDelegationStatus::Rejected,
        "cancelled" => PersistedDelegationStatus::Cancelled,
        _ => return None,
    };
    let target_kind = match value.get("target_kind")?.as_str()? {
        "agent" => ProfileKind::Agent,
        "team" => ProfileKind::Team,
        _ => return None,
    };
    let requesting_profile_id =
        ProfileId::new(value.get("requesting_profile_id")?.as_str()?.to_owned()).ok()?;
    let target_id = ProfileId::new(value.get("target_id")?.as_str()?.to_owned()).ok()?;
    let summary = value
        .get("final_text")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("message"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    Some(TerminalDelegationUpdate {
        requesting_profile_id,
        target_kind,
        target_id,
        task: value.get("task")?.as_str()?.to_owned(),
        status,
        child_operation_id: value
            .get("child_operation_id")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        summary,
    })
}

fn persisted_content_blocks_from_invocation(
    invocation: &PromptInvocation,
) -> Result<Vec<PersistedContentBlock>, CodingSessionError> {
    match invocation {
        PromptInvocation::Text(text) if !text.is_empty() => {
            Ok(vec![PersistedContentBlock::Text { text: text.clone() }])
        }
        PromptInvocation::Text(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty text input".into(),
        }),
        PromptInvocation::Content(content) if !content.is_empty() => {
            Ok(content.iter().map(persisted_content_block).collect())
        }
        PromptInvocation::Content(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty content input".into(),
        }),
        PromptInvocation::Skill {
            name,
            additional_instructions,
        } => {
            let text = match additional_instructions {
                Some(instructions) if !instructions.is_empty() => {
                    format!("skill:{name}\n{instructions}")
                }
                _ => format!("skill:{name}"),
            };
            Ok(vec![PersistedContentBlock::Text { text }])
        }
        PromptInvocation::PromptTemplate { name, args } => {
            let text = if args.is_empty() {
                format!("prompt_template:{name}")
            } else {
                format!("prompt_template:{name}\n{}", args.join("\n"))
            };
            Ok(vec![PersistedContentBlock::Text { text }])
        }
        PromptInvocation::Compact { .. } => Err(CodingSessionError::UnsupportedCapability {
            capability: "manual compaction in PromptTurnRunner".into(),
        }),
    }
}

fn persisted_content_block(content: &ContentBlock) -> PersistedContentBlock {
    match content {
        ContentBlock::Text { text, .. } => PersistedContentBlock::Text { text: text.clone() },
        ContentBlock::Image { mime_type, data } => PersistedContentBlock::Image {
            mime_type: mime_type.clone(),
            data: data.clone(),
        },
        ContentBlock::Thinking {
            thinking,
            thinking_signature,
            provider_metadata,
            redacted,
        } => PersistedContentBlock::Thinking {
            thinking: thinking.clone(),
            thinking_signature: thinking_signature.clone(),
            provider_metadata: provider_metadata.clone(),
            redacted: *redacted,
        },
        ContentBlock::ToolCall {
            name, arguments, ..
        } => PersistedContentBlock::Text {
            text: format!("[tool_call:{name} {arguments}]"),
        },
        ContentBlock::ProviderItem { api, item } => PersistedContentBlock::ProviderItem {
            api: api.clone(),
            item: item.clone(),
        },
    }
}

fn persisted_assistant_content_blocks(content: &[ContentBlock]) -> Vec<PersistedContentBlock> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => {
                Some(PersistedContentBlock::Text { text: text.clone() })
            }
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                provider_metadata,
                redacted,
            } => Some(PersistedContentBlock::Thinking {
                thinking: thinking.clone(),
                thinking_signature: thinking_signature.clone(),
                provider_metadata: provider_metadata.clone(),
                redacted: *redacted,
            }),
            ContentBlock::Image { mime_type, data } => Some(PersistedContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            }),
            ContentBlock::ToolCall { .. } => None,
            ContentBlock::ProviderItem { api, item } => Some(PersistedContentBlock::ProviderItem {
                api: api.clone(),
                item: item.clone(),
            }),
        })
        .collect()
}

fn persisted_content_blocks_text(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => text.clone(),
            PersistedContentBlock::Thinking { thinking, .. } => thinking.clone(),
            PersistedContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            PersistedContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_blocks_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.clone(),
            ContentBlock::Thinking { thinking, .. } => thinking.clone(),
            ContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            ContentBlock::ToolCall { name, .. } => format!("[tool_call:{name}]"),
            ContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
