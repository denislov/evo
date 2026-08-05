use std::path::Path;

use agent_core::api::agent::AgentResources;
use serde::{Deserialize, Serialize};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use tool_contract::api::definition::ToolId;

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::application::capability::{OperationCapabilitySnapshot, tool_uses_filesystem};
use crate::kernel::capability::{ActorId, ModelCapability, ToolCapabilitySet};
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::{
    DelegationRequest, DelegationToolExecutor, PromptTurnMode, PromptTurnOptions,
};
use crate::profiles::{
    AgentProfile, DelegationConfirmationMode, DelegationPolicy, ProfileId, ProfileKind,
    ProfileRegistry,
};
use crate::session::event::PersistedDelegationRuntimeSeed;
use crate::session::replay::ReplayPendingDelegationConfirmation;

const DELEGATION_CONFIRMATION_TTL_HOURS: i64 = 24;

#[cfg(test)]
mod capability_snapshot_tests;
pub(crate) mod confirmation;
pub(crate) mod execution;
mod tool;
pub(crate) use tool::delegation_tools;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DelegationTargetInventory {
    agents: Vec<DelegationTarget>,
    teams: Vec<DelegationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DelegationTarget {
    id: ProfileId,
    display_name: String,
    description: Option<String>,
}

impl DelegationTargetInventory {
    pub(crate) fn from_registry(registry: &ProfileRegistry, policy: &DelegationPolicy) -> Self {
        let agents = registry
            .agents()
            .filter(|profile| target_is_allowed(&policy.allowed_agents, &profile.id))
            .map(|profile| DelegationTarget {
                id: profile.id.clone(),
                display_name: profile.display_name.clone(),
                description: profile.description.clone(),
            })
            .collect();
        let teams = registry
            .teams()
            .filter(|profile| target_is_allowed(&policy.allowed_teams, &profile.id))
            .map(|profile| DelegationTarget {
                id: profile.id.clone(),
                display_name: profile.display_name.clone(),
                description: profile.description.clone(),
            })
            .collect();
        Self { agents, teams }
    }

    pub(crate) fn agent_ids(&self) -> impl Iterator<Item = &ProfileId> {
        self.agents.iter().map(|target| &target.id)
    }

    pub(crate) fn team_ids(&self) -> impl Iterator<Item = &ProfileId> {
        self.teams.iter().map(|target| &target.id)
    }
}

pub(crate) fn delegation_tool_ids(policy: &DelegationPolicy) -> impl Iterator<Item = ToolId> {
    [
        policy
            .allow_delegate_agent
            .then(|| ToolId::new("delegate_agent").expect("static tool id is valid")),
        policy
            .allow_delegate_team
            .then(|| ToolId::new("delegate_team").expect("static tool id is valid")),
    ]
    .into_iter()
    .flatten()
}

pub(crate) fn capability_snapshot_for_delegated_profile(
    parent: &OperationCapabilitySnapshot,
    operation_id: impl Into<String>,
    profile: &AgentProfile,
    actor: ActorId,
) -> OperationCapabilitySnapshot {
    let mut released_tools = profile
        .tools
        .iter()
        .filter(|id| parent.tools.allows(id))
        .cloned()
        .collect::<Vec<_>>();
    for tool_id in delegation_tool_ids(&profile.delegation) {
        if parent.tools.allows(&tool_id) && !released_tools.iter().any(|id| id == &tool_id) {
            released_tools.push(tool_id);
        }
    }
    // Server-side tools follow the child automatically, but never wider than
    // the parent: a delegate cannot reach the network its parent could not.
    if !released_tools.is_empty() {
        for tool_id in crate::tools::server_tool_ids() {
            if parent.tools.allows(&tool_id) && !released_tools.iter().any(|id| id == &tool_id) {
                released_tools.push(tool_id);
            }
        }
    }
    let releases_filesystem = released_tools.iter().any(tool_uses_filesystem);
    OperationCapabilitySnapshot {
        generation: parent.generation,
        operation_id: operation_id.into(),
        actor,
        model: Some(ModelCapability {
            profile_id: Some(profile.id.clone()),
        }),
        tools: ToolCapabilitySet::from_ids(released_tools),
        commands: Default::default(),
        filesystem: parent.filesystem.clone().filter(|_| releases_filesystem),
        shell: parent
            .shell
            .clone()
            .filter(|_| profile.tools.iter().any(|id| id.as_str() == "bash")),
        session_read: None,
        session_write: None,
        ui: None,
    }
}

pub(crate) fn capability_snapshot_for_child_operation(
    parent: &OperationCapabilitySnapshot,
    operation_id: impl Into<String>,
) -> OperationCapabilitySnapshot {
    let mut child = parent.clone();
    child.operation_id = operation_id.into();
    child.actor = ActorId::ChildOperation(parent.operation_id.clone());
    child.session_read = None;
    child.session_write = None;
    child.ui = None;
    child
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DelegationAuthorizationDecision {
    Approved {
        request: DelegationRequest,
        child_delegation_depth: usize,
    },
    RequiresConfirmation {
        request: DelegationRequest,
        reason: String,
        child_delegation_depth: usize,
    },
    Rejected {
        request: DelegationRequest,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DelegationToolResultStatus {
    Completed,
    Failed,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DelegationToolResult {
    pub(crate) status: DelegationToolResultStatus,
    pub(crate) target_kind: ProfileKind,
    pub(crate) target_id: ProfileId,
    pub(crate) task: String,
    pub(crate) requesting_profile_id: ProfileId,
    pub(crate) operation_id: String,
    pub(crate) tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) child_operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) final_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl DelegationToolResult {
    pub(crate) fn from_request(
        request: &DelegationRequest,
        status: DelegationToolResultStatus,
    ) -> Self {
        Self {
            status,
            target_kind: request.target_kind,
            target_id: request.target_id.clone(),
            task: request.task.clone(),
            requesting_profile_id: request.requesting_profile_id.clone(),
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            child_operation_id: None,
            final_text: None,
            error: None,
        }
    }

    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).expect("typed delegation tool result is serializable")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DelegationLineageEntry {
    pub(crate) kind: ProfileKind,
    pub(crate) id: ProfileId,
}

impl DelegationLineageEntry {
    pub(crate) fn new(kind: ProfileKind, id: impl Into<ProfileId>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub(crate) fn agent(id: impl Into<ProfileId>) -> Self {
        Self::new(ProfileKind::Agent, id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelegationConfirmation {
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub requesting_profile_id: ProfileId,
    pub target_kind: ProfileKind,
    pub target_id: ProfileId,
    pub task: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingDelegationConfirmationState {
    pub(crate) request: DelegationRequest,
    pub(crate) prompt_options: PromptTurnOptions,
    pub(crate) reason: String,
    pub(crate) requested_at: String,
    pub(crate) child_delegation_depth: usize,
    pub(crate) delegation_lineage: Vec<DelegationLineageEntry>,
}

impl PendingDelegationConfirmationState {
    pub(crate) fn is_active_at(&self, now: &str) -> bool {
        !delegation_confirmation_is_expired(&self.requested_at, now)
    }

    pub(crate) fn view(&self) -> PendingDelegationConfirmation {
        PendingDelegationConfirmation {
            operation_id: self.request.operation_id.clone(),
            turn_id: self.request.turn_id.clone(),
            tool_call_id: self.request.tool_call_id.clone(),
            requesting_profile_id: self.request.requesting_profile_id.clone(),
            target_kind: self.request.target_kind,
            target_id: self.request.target_id.clone(),
            task: self.request.task.clone(),
            reason: self.reason.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct PendingDelegationConfirmationQueue {
    pending: Vec<PendingDelegationConfirmationState>,
}

impl PendingDelegationConfirmationQueue {
    pub(crate) fn from_pending(pending: Vec<PendingDelegationConfirmationState>) -> Self {
        Self { pending }
    }

    pub(crate) fn is_duplicate(&self, pending: &PendingDelegationConfirmationState) -> bool {
        self.pending.iter().any(|existing| {
            existing.request.operation_id == pending.request.operation_id
                && existing.request.tool_call_id == pending.request.tool_call_id
        })
    }

    pub(crate) fn push(&mut self, pending: PendingDelegationConfirmationState) {
        self.pending.push(pending);
    }

    pub(crate) fn active_views(&self, now: &str) -> Vec<PendingDelegationConfirmation> {
        self.pending
            .iter()
            .filter(|pending| pending.is_active_at(now))
            .map(PendingDelegationConfirmationState::view)
            .collect()
    }

    pub(crate) fn active_pending(
        &self,
        operation_id: &str,
        tool_call_id: &str,
        now: &str,
    ) -> Option<&PendingDelegationConfirmationState> {
        self.pending.iter().find(|pending| {
            pending.is_active_at(now)
                && pending.request.operation_id == operation_id
                && pending.request.tool_call_id == tool_call_id
        })
    }

    pub(crate) fn remove_active(
        &mut self,
        operation_id: &str,
        tool_call_id: &str,
        now: &str,
    ) -> Option<PendingDelegationConfirmationState> {
        let index = self.pending.iter().position(|pending| {
            pending.is_active_at(now)
                && pending.request.operation_id == operation_id
                && pending.request.tool_call_id == tool_call_id
        })?;
        Some(self.pending.remove(index))
    }
}

fn delegation_confirmation_is_expired(requested_at: &str, now: &str) -> bool {
    let Ok(requested_at) = OffsetDateTime::parse(requested_at, &Rfc3339) else {
        return false;
    };
    let Ok(now) = OffsetDateTime::parse(now, &Rfc3339) else {
        return false;
    };
    now >= requested_at + TimeDuration::hours(DELEGATION_CONFIRMATION_TTL_HOURS)
}

pub(crate) fn pending_state_from_replay(
    replay_pending: ReplayPendingDelegationConfirmation,
    cwd: &Path,
) -> Result<PendingDelegationConfirmationState, CodingSessionError> {
    let child_delegation_depth = replay_pending
        .runtime_seed
        .parent_delegation_depth
        .saturating_add(1);
    let delegation_lineage = replay_pending.runtime_seed.delegation_lineage.clone();
    Ok(PendingDelegationConfirmationState {
        request: DelegationRequest {
            operation_id: replay_pending.source_operation_id,
            turn_id: replay_pending.turn_id,
            tool_call_id: replay_pending.tool_call_id,
            requesting_profile_id: replay_pending.requesting_profile_id,
            target_kind: replay_pending.target_kind,
            target_id: replay_pending.target_id,
            task: replay_pending.task,
        },
        prompt_options: prompt_options_from_delegation_runtime_seed(
            replay_pending.runtime_seed,
            cwd,
        )?,
        reason: replay_pending.reason,
        requested_at: replay_pending.requested_at,
        child_delegation_depth,
        delegation_lineage,
    })
}

fn prompt_options_from_delegation_runtime_seed(
    seed: PersistedDelegationRuntimeSeed,
    cwd: &Path,
) -> Result<PromptTurnOptions, CodingSessionError> {
    let (config, mut diagnostics) = crate::config::load_config(cwd);
    let resolved_api_key = crate::config::auth::resolve_api_key(
        &seed.model.provider,
        None,
        &config.auth,
        &mut diagnostics,
    );
    let auth_diagnostics = resolved_api_key
        .as_ref()
        .map(|key| key.provider_auth_diagnostic())
        .into_iter()
        .collect();
    let api_key = resolved_api_key.map(|key| key.value);
    let tools = Vec::new();
    let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: seed.model,
        api_key,
        auth_diagnostics,
        system_prompt: seed.system_prompt,
        max_turns: seed.max_turns,
        tools,
        register_builtins: seed.register_builtins,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(cwd.to_path_buf())),
        session_target: None,
        session_name: seed.session_name,
        thinking_level: parse_optional_runtime_value("thinking level", seed.thinking_level)?,
        tool_execution: parse_optional_runtime_value("tool execution mode", seed.tool_execution)?,
        resources: AgentResources::default(),
        settings: Some(config.settings),
        invocation: PromptInvocation::Text(String::new()),
    })
    .with_mode(parse_prompt_turn_mode(&seed.mode)?);
    Ok(options)
}

fn parse_optional_runtime_value<T>(
    label: &str,
    value: Option<String>,
) -> Result<Option<T>, CodingSessionError>
where
    T: std::str::FromStr<Err = String>,
{
    value
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|message| CodingSessionError::Session {
                    message: format!("invalid persisted delegation {label}: {message}"),
                })
        })
        .transpose()
}

fn parse_prompt_turn_mode(value: &str) -> Result<PromptTurnMode, CodingSessionError> {
    match value {
        "print" => Ok(PromptTurnMode::Print),
        "json" => Ok(PromptTurnMode::Json),
        "rpc" => Ok(PromptTurnMode::Rpc),
        other => Err(CodingSessionError::Session {
            message: format!("invalid persisted delegation prompt mode: {other}"),
        }),
    }
}

fn prompt_turn_mode_label(mode: PromptTurnMode) -> &'static str {
    match mode {
        PromptTurnMode::Print => "print",
        PromptTurnMode::Json => "json",
        PromptTurnMode::Rpc => "rpc",
    }
}

fn persisted_delegation_model(
    model: &ai_protocol::api::model::Model,
) -> ai_protocol::api::model::Model {
    let mut persisted = model.clone();
    persisted.headers = None;
    persisted
}

pub(crate) fn delegation_runtime_seed_from_prompt_options(
    options: &PromptTurnOptions,
    child_delegation_depth: usize,
    delegation_lineage: &[DelegationLineageEntry],
) -> Result<PersistedDelegationRuntimeSeed, CodingSessionError> {
    let runtime = options
        .runtime()
        .ok_or_else(|| CodingSessionError::Config {
            message: "delegation confirmation options do not include a runtime snapshot".into(),
        })?;
    Ok(PersistedDelegationRuntimeSeed {
        mode: prompt_turn_mode_label(options.mode()).to_string(),
        model: persisted_delegation_model(runtime.model()),
        system_prompt: runtime.system_prompt().map(str::to_owned),
        max_turns: runtime.max_turns(),
        tool_names: runtime.all_tool_names(),
        register_builtins: runtime.register_builtins(),
        thinking_level: runtime.thinking_level().map(|level| level.to_string()),
        tool_execution: runtime.tool_execution().map(|mode| mode.to_string()),
        session_name: options.session_name().map(str::to_owned),
        parent_delegation_depth: child_delegation_depth.saturating_sub(1),
        delegation_lineage: delegation_lineage.to_vec(),
    })
}

pub(crate) fn delegation_lineage_for_request(
    parent_lineage: &[DelegationLineageEntry],
    request: &DelegationRequest,
) -> Vec<DelegationLineageEntry> {
    let mut lineage = parent_lineage.to_vec();
    push_unique_lineage_entry(
        &mut lineage,
        DelegationLineageEntry::agent(request.requesting_profile_id.clone()),
    );
    push_unique_lineage_entry(
        &mut lineage,
        DelegationLineageEntry::new(request.target_kind, request.target_id.clone()),
    );
    lineage
}

fn push_unique_lineage_entry(
    lineage: &mut Vec<DelegationLineageEntry>,
    entry: DelegationLineageEntry,
) {
    if !lineage.iter().any(|existing| existing == &entry) {
        lineage.push(entry);
    }
}

pub(crate) fn authorize_delegation_requests_with_lineage(
    requests: &[DelegationRequest],
    policy: &DelegationPolicy,
    current_depth: usize,
    lineage: &[DelegationLineageEntry],
) -> Vec<DelegationAuthorizationDecision> {
    let mut accepted_children = 0usize;
    requests
        .iter()
        .map(|request| {
            let mut effective_lineage = lineage.to_vec();
            push_unique_lineage_entry(
                &mut effective_lineage,
                DelegationLineageEntry::agent(request.requesting_profile_id.clone()),
            );
            if let Some(reason) = rejection_reason(
                request,
                policy,
                current_depth,
                accepted_children,
                &effective_lineage,
            ) {
                return DelegationAuthorizationDecision::Rejected {
                    request: request.clone(),
                    reason,
                };
            }
            accepted_children += 1;
            let child_delegation_depth = current_depth.saturating_add(1);
            if let Some(reason) = confirmation_reason(request, policy) {
                DelegationAuthorizationDecision::RequiresConfirmation {
                    request: request.clone(),
                    reason,
                    child_delegation_depth,
                }
            } else {
                DelegationAuthorizationDecision::Approved {
                    request: request.clone(),
                    child_delegation_depth,
                }
            }
        })
        .collect()
}

fn rejection_reason(
    request: &DelegationRequest,
    policy: &DelegationPolicy,
    current_depth: usize,
    accepted_children: usize,
    lineage: &[DelegationLineageEntry],
) -> Option<String> {
    if current_depth >= policy.max_depth {
        return Some("delegation policy max_depth is exhausted".into());
    }
    if accepted_children >= policy.max_parallel_children {
        return Some("delegation policy max_parallel_children is exhausted".into());
    }
    if lineage
        .iter()
        .any(|entry| entry.kind == request.target_kind && entry.id == request.target_id)
    {
        return Some(format!(
            "delegation cycle detected for {} target {}",
            profile_kind_label(request.target_kind),
            request.target_id
        ));
    }
    match request.target_kind {
        ProfileKind::Agent if !policy.allow_delegate_agent => {
            Some("delegation policy does not allow agent delegation".into())
        }
        ProfileKind::Agent if !target_is_allowed(&policy.allowed_agents, &request.target_id) => {
            Some("target agent is not allowed by delegation policy".into())
        }
        ProfileKind::Team if !policy.allow_delegate_team => {
            Some("delegation policy does not allow team delegation".into())
        }
        ProfileKind::Team if !target_is_allowed(&policy.allowed_teams, &request.target_id) => {
            Some("target team is not allowed by delegation policy".into())
        }
        _ => None,
    }
}

fn profile_kind_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}

fn confirmation_reason(request: &DelegationRequest, policy: &DelegationPolicy) -> Option<String> {
    match policy.require_confirmation {
        DelegationConfirmationMode::Never => None,
        DelegationConfirmationMode::Always => {
            Some("delegation policy requires confirmation".into())
        }
        DelegationConfirmationMode::Writes if request.target_kind == ProfileKind::Team => {
            Some("team delegation requires confirmation under writes policy".into())
        }
        DelegationConfirmationMode::Writes => None,
    }
}

fn handle_delegation_request(
    target_kind: &str,
    target_field: &str,
    profile_id: &ProfileId,
    policy: &DelegationPolicy,
    args: serde_json::Value,
) -> Result<String, String> {
    let target = required_profile_id(&args, target_field)?;
    let task = required_non_empty_string(&args, "task")?;
    if policy.max_depth == 0 {
        return Ok(delegation_response(
            "rejected",
            target_kind,
            &target,
            &task,
            Some("delegation policy max_depth is 0"),
            profile_id,
        ));
    }
    let allowed = match target_kind {
        "agent" => target_is_allowed(&policy.allowed_agents, &target),
        "team" => target_is_allowed(&policy.allowed_teams, &target),
        _ => false,
    };
    if !allowed {
        return Ok(delegation_response(
            "rejected",
            target_kind,
            &target,
            &task,
            Some("target is not allowed by delegation policy"),
            profile_id,
        ));
    }
    Ok(delegation_response(
        "requested",
        target_kind,
        &target,
        &task,
        Some("delegation request captured for session-owned authorization"),
        profile_id,
    ))
}

pub(super) fn required_profile_id(
    args: &serde_json::Value,
    key: &str,
) -> Result<ProfileId, String> {
    let value = required_non_empty_string(args, key)?;
    ProfileId::new(value).map_err(|error| format!("invalid {key}: {error}"))
}

pub(super) fn required_non_empty_string(
    args: &serde_json::Value,
    key: &str,
) -> Result<String, String> {
    let value = args
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if value.is_empty() {
        return Err(format!("delegation request requires non-empty {key}"));
    }
    Ok(value.to_string())
}

fn target_is_allowed(allowed: &[ProfileId], target: &ProfileId) -> bool {
    allowed.is_empty() || allowed.iter().any(|allowed| allowed == target)
}

pub(super) fn delegation_response(
    status: &str,
    target_kind: &str,
    target: &ProfileId,
    task: &str,
    message: Option<&str>,
    profile_id: &ProfileId,
) -> String {
    let mut response = serde_json::json!({
        "status": status,
        "target_kind": target_kind,
        "target_id": target.as_str(),
        "task": task,
        "requesting_profile_id": profile_id.as_str(),
    });
    if let Some(message) = message {
        response["message"] = serde_json::Value::String(message.to_string());
    }
    response.to_string()
}
