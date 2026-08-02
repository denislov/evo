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
use crate::mutex::MutexExt;

use crate::application::capability::OperationCapabilitySnapshot;
use crate::events::prompt_stream::PromptStreamEvent;
use crate::kernel::control::PromptControlReceiver;
use crate::kernel::error::CodingSessionError;
use crate::operations::delegation::{
    DelegationAuthorizationDecision, DelegationLineageEntry, DelegationTargetInventory,
    PendingDelegationConfirmationState, authorize_delegation_requests_with_lineage,
};
use crate::platform::time::{SystemClock, SystemIdGenerator};
use crate::profiles::{AgentProfile, DelegationPolicy, ProfileId, ProfileKind, ProfileRegistry};
use crate::services::authorization::{AuthorizationHookContext, AuthorizationService};
use crate::services::event::{AgentEventMappingContext, EventService, map_agent_event};
use crate::services::ports::SessionWriterPort;
use crate::session::event::{
    DiagnosticLevel, PersistedContentBlock, PersistedDelegationStatus, PersistedToolResult,
};
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

        // Server-side tools are declared, not executed here, so they are not
        // part of the caller-supplied inventory (which is capability-bound to a
        // cwd). Add them once, at the single point where the resolved model is
        // known, so the policy chain, the delegation seed, and the outgoing
        // tool list all see the same set. Callers that already supplied one
        // win, keeping this idempotent across restore paths.
        //
        // An empty inventory is left empty. `filter_tools` has already run by
        // this point, so `--no-tools` arrives here as an empty list; adding a
        // provider-side tool back would reopen a door the caller shut. Same
        // rule as `grant_server_tools` applies to the name lists.
        let mut tools = tools;
        if !tools.is_empty() {
            for tool in crate::tools::server_side_tools(&model) {
                if !tools.iter().any(|existing| existing.name == tool.name) {
                    tools.push(tool);
                }
            }
        }

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
        self.profile_tool_allowlist = (!profile.tools.is_empty()).then(|| {
            let mut tools = profile.tools.clone();
            crate::tools::grant_server_tools(&mut tools);
            tools
        });
        self.profile_skill_allowlist = (!profile.skills.is_empty()).then(|| profile.skills.clone());
    }

    pub(crate) fn apply_delegated_agent_profile(
        &mut self,
        profile: &AgentProfile,
        registry: &ProfileRegistry,
        diagnostics: Vec<CodingDiagnostic>,
    ) {
        self.apply_agent_profile_core(profile, registry, diagnostics);
        self.profile_tool_allowlist = Some({
            let mut tools = profile.tools.clone();
            crate::tools::grant_server_tools(&mut tools);
            tools
        });
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
    authorization_event_writer: Option<SessionWriterPort>,
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

mod finalize;
mod setup;
mod stream;

use stream::{ReasoningDurationTracker, persisted_content_blocks_from_invocation};

#[cfg(test)]
mod transition_table_tests {
    use super::*;
    use crate::kernel::error::SessionWriteFailureReason;

    fn context() -> PromptTurnContext {
        PromptTurnContext::new(
            PromptTurnIds::new("operation", "turn"),
            PromptTurnOptions::new(PromptInvocation::Text("test".into())),
        )
    }

    fn assistant_message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::empty("test-api", "test-model");
        message.content.push(ContentBlock::Text {
            text: text.into(),
            text_signature: None,
        });
        message
    }

    #[test]
    fn prompt_input_preparation_transition_table() {
        #[derive(Debug)]
        enum Expected {
            Text(&'static str),
            Error(&'static str),
        }

        let cases = [
            (
                "text",
                PromptInvocation::Text("hello".into()),
                Expected::Text("hello"),
            ),
            (
                "empty text",
                PromptInvocation::Text(String::new()),
                Expected::Error("input"),
            ),
            (
                "content",
                PromptInvocation::Content(vec![ContentBlock::Text {
                    text: "content".into(),
                    text_signature: None,
                }]),
                Expected::Text("content"),
            ),
            (
                "empty content",
                PromptInvocation::Content(Vec::new()),
                Expected::Error("input"),
            ),
            (
                "skill",
                PromptInvocation::Skill {
                    name: "review".into(),
                    additional_instructions: Some("focus on safety".into()),
                },
                Expected::Text("skill:review\nfocus on safety"),
            ),
            (
                "prompt template",
                PromptInvocation::PromptTemplate {
                    name: "release".into(),
                    args: vec!["v1".into(), "stable".into()],
                },
                Expected::Text("prompt_template:release\nv1\nstable"),
            ),
            (
                "manual compaction",
                PromptInvocation::Compact {
                    custom_instructions: None,
                },
                Expected::Error("unsupported_capability"),
            ),
        ];

        for (name, invocation, expected) in cases {
            match (
                persisted_content_blocks_from_invocation(&invocation),
                expected,
            ) {
                (Ok(blocks), Expected::Text(expected_text)) => assert!(
                    matches!(
                        blocks.as_slice(),
                        [PersistedContentBlock::Text { text }] if text == expected_text
                    ),
                    "{name}: {blocks:?}"
                ),
                (Err(error), Expected::Error(expected_code)) => {
                    assert_eq!(error.code(), expected_code, "{name}")
                }
                (actual, expected) => panic!("{name}: expected {expected:?}, got {actual:?}"),
            }
        }
    }

    #[test]
    fn prompt_completion_recording_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum Action {
            Complete,
            RecordFinal,
        }

        let mut context = context();
        let cases = [
            (Action::Complete, false, false),
            (Action::RecordFinal, true, false),
            (Action::Complete, true, true),
            (Action::Complete, true, true),
        ];

        for (action, expected_ok, expected_recorded) in cases {
            let result = match action {
                Action::Complete => context.record_prompt_completed(),
                Action::RecordFinal => {
                    context.record_final_message(assistant_message("done"));
                    Ok(())
                }
            };
            assert_eq!(result.is_ok(), expected_ok, "{action:?}");
            assert_eq!(context.completion_recorded, expected_recorded, "{action:?}");
        }
    }

    #[test]
    fn prompt_outcome_transition_table() {
        #[derive(Debug, Clone, Copy)]
        enum Action {
            SuccessWithoutMessage,
            Success,
            Abort,
            Fail,
            FailQueueSaturated,
        }

        #[derive(Debug, Clone, Copy)]
        enum Expected {
            Error,
            Success,
            Aborted,
            Failed { diagnostics: usize },
        }

        let cases = [
            (Action::SuccessWithoutMessage, Expected::Error),
            (Action::Success, Expected::Success),
            (Action::Abort, Expected::Aborted),
            (Action::Fail, Expected::Failed { diagnostics: 0 }),
            (
                Action::FailQueueSaturated,
                Expected::Failed { diagnostics: 1 },
            ),
        ];

        for (action, expected) in cases {
            let mut context = context();
            let outcome = match action {
                Action::SuccessWithoutMessage => context.finish_success(None, None),
                Action::Success => {
                    context.record_final_message(assistant_message("done"));
                    context.finish_success(Some("session".into()), Some("leaf".into()))
                }
                Action::Abort => Ok(context.finish_abort("cancelled", Some("session".into()))),
                Action::Fail => Ok(context.finish_failure(CodingSessionError::Provider {
                    message: "provider failed".into(),
                })),
                Action::FailQueueSaturated => Ok(context.finish_failure(
                    CodingSessionError::SessionWriteFailure {
                        reason: SessionWriteFailureReason::QueueSaturated,
                        message: "writer queue is full".into(),
                    },
                )),
            };

            match (outcome, expected) {
                (Err(_), Expected::Error) => {}
                (
                    Ok(InternalPromptTurnOutcome::Success {
                        final_text,
                        session_id,
                        leaf_id,
                        ..
                    }),
                    Expected::Success,
                ) => {
                    assert_eq!(final_text, "done");
                    assert_eq!(session_id.as_deref(), Some("session"));
                    assert_eq!(leaf_id.as_deref(), Some("leaf"));
                }
                (Ok(InternalPromptTurnOutcome::Aborted { reason, .. }), Expected::Aborted) => {
                    assert_eq!(reason, "cancelled")
                }
                (
                    Ok(InternalPromptTurnOutcome::Failed { diagnostics, .. }),
                    Expected::Failed {
                        diagnostics: expected_diagnostics,
                    },
                ) => assert_eq!(diagnostics.len(), expected_diagnostics),
                (actual, expected) => {
                    panic!("{action:?}: expected {expected:?}, got {actual:?}")
                }
            }
        }
    }

    #[test]
    fn queue_saturation_adds_an_operation_diagnostic() {
        let context = PromptTurnContext::new(
            PromptTurnIds::new("operation-queue-saturated", "turn-queue-saturated"),
            PromptTurnOptions::new(PromptInvocation::Text("test".into())),
        );
        let outcome = context.finish_failure(CodingSessionError::SessionWriteFailure {
            reason: SessionWriteFailureReason::QueueSaturated,
            message: "bounded queue timeout".into(),
        });
        let InternalPromptTurnOutcome::Failed { diagnostics, .. } = outcome else {
            panic!("queue saturation must remain a typed failed prompt outcome");
        };
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("Session persistence is lagging")
        }));
    }
}
