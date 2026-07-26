use std::collections::BTreeSet;
use std::path::Path;

use agent_core::api::agent::AgentResources;
use agent_core::api::tool::AgentTool;
use serde::{Deserialize, Serialize};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::operations::prompt::context::{
    DelegationRequest, DelegationToolExecutor, PromptTurnMode, PromptTurnOptions,
};
use crate::profiles::{
    AgentProfile, DelegationConfirmationMode, DelegationPolicy, ProfileId, ProfileKind,
    ProfileRegistry,
};
use crate::runtime::capability::{
    ActorId, ModelCapability, OperationCapabilitySnapshot, ToolCapabilitySet, tool_uses_filesystem,
};
use crate::runtime::error::CodingSessionError;
use crate::session::event::PersistedDelegationRuntimeSeed;
use crate::session::replay::ReplayPendingDelegationConfirmation;

const DELEGATION_CONFIRMATION_TTL_HOURS: i64 = 24;

pub(crate) mod confirmation;
pub(crate) mod execution;

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

pub(crate) fn delegation_tools(
    profile_id: Option<&ProfileId>,
    policy: Option<&DelegationPolicy>,
    inventory: &DelegationTargetInventory,
    executor: Option<DelegationToolExecutor>,
) -> Vec<AgentTool> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    let profile_id = profile_id
        .cloned()
        .unwrap_or_else(|| ProfileId::from("default"));
    let mut tools = Vec::new();
    if policy.allow_delegate_agent && !inventory.agents.is_empty() {
        tools.push(delegate_agent_tool(
            profile_id.clone(),
            policy.clone(),
            &inventory.agents,
            executor.clone(),
        ));
    }
    if policy.allow_delegate_team && !inventory.teams.is_empty() {
        tools.push(delegate_team_tool(
            profile_id,
            policy.clone(),
            &inventory.teams,
            executor,
        ));
    }
    tools
}

pub(crate) fn delegation_tool_names(
    policy: &DelegationPolicy,
) -> impl Iterator<Item = &'static str> {
    [
        policy.allow_delegate_agent.then_some("delegate_agent"),
        policy.allow_delegate_team.then_some("delegate_team"),
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
        .filter(|name| parent.tools.allows(name))
        .cloned()
        .collect::<Vec<_>>();
    for tool_name in delegation_tool_names(&profile.delegation) {
        if parent.tools.allows(tool_name) && !released_tools.iter().any(|name| name == tool_name) {
            released_tools.push(tool_name.to_owned());
        }
    }
    let releases_filesystem = released_tools.iter().any(|name| tool_uses_filesystem(name));
    OperationCapabilitySnapshot {
        generation: parent.generation,
        operation_id: operation_id.into(),
        actor,
        model: Some(ModelCapability {
            profile_id: Some(profile.id.clone()),
        }),
        tools: ToolCapabilitySet::from_names(released_tools),
        commands: Default::default(),
        filesystem: parent.filesystem.clone().filter(|_| releases_filesystem),
        shell: parent
            .shell
            .clone()
            .filter(|_| profile.tools.iter().any(|name| name == "bash")),
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
    let tools = restored_builtin_tools(cwd, &seed.tool_names)?;
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

fn restored_builtin_tools(
    cwd: &Path,
    tool_names: &[String],
) -> Result<Vec<AgentTool>, CodingSessionError> {
    if tool_names.is_empty() {
        return Ok(Vec::new());
    }
    let names = tool_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    Ok(crate::tools::builtin_tools(cwd.to_path_buf())?
        .into_iter()
        .filter(|tool| names.contains(tool.name.as_str()))
        .collect())
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

fn persisted_delegation_model(model: &ai::api::model::Model) -> ai::api::model::Model {
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
        tool_names: runtime
            .tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect(),
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

#[cfg(test)]
pub(crate) fn authorize_delegation_requests(
    requests: &[DelegationRequest],
    policy: &DelegationPolicy,
    current_depth: usize,
) -> Vec<DelegationAuthorizationDecision> {
    authorize_delegation_requests_with_lineage(requests, policy, current_depth, &[])
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

fn delegate_agent_tool(
    profile_id: ProfileId,
    policy: DelegationPolicy,
    targets: &[DelegationTarget],
    executor: Option<DelegationToolExecutor>,
) -> AgentTool {
    AgentTool::new_text(
        "delegate_agent",
        delegation_description(
            "Request bounded help from another configured agent profile. The session owner validates policy before any child work is allowed.",
            "agent",
            targets,
        ),
        delegation_parameters(
            "agent_id",
            "agent",
            targets,
            matches!(
                policy.require_confirmation,
                DelegationConfirmationMode::Always
            ),
        ),
        move |context, args| {
            let profile_id = profile_id.clone();
            let policy = policy.clone();
            let executor = executor.clone();
            async move {
                match executor {
                    Some(executor) => executor(context, args).await,
                    None => {
                        handle_delegation_request("agent", "agent_id", &profile_id, &policy, args)
                    }
                }
            }
        },
    )
}

fn delegate_team_tool(
    profile_id: ProfileId,
    policy: DelegationPolicy,
    targets: &[DelegationTarget],
    executor: Option<DelegationToolExecutor>,
) -> AgentTool {
    AgentTool::new_text(
        "delegate_team",
        delegation_description(
            "Request bounded help from a configured team profile. The session owner validates policy before any child work is allowed.",
            "team",
            targets,
        ),
        delegation_parameters(
            "team_id",
            "team",
            targets,
            !matches!(
                policy.require_confirmation,
                DelegationConfirmationMode::Never
            ),
        ),
        move |context, args| {
            let profile_id = profile_id.clone();
            let policy = policy.clone();
            let executor = executor.clone();
            async move {
                match executor {
                    Some(executor) => executor(context, args).await,
                    None => {
                        handle_delegation_request("team", "team_id", &profile_id, &policy, args)
                    }
                }
            }
        },
    )
}

fn delegation_description(base: &str, kind: &str, targets: &[DelegationTarget]) -> String {
    let inventory = targets
        .iter()
        .map(|target| {
            let display_name = single_line(&target.display_name);
            match target.description.as_deref().map(single_line) {
                Some(description) if !description.is_empty() => {
                    format!("- {}: {} - {}", target.id, display_name, description)
                }
                _ => format!("- {}: {}", target.id, display_name),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{base}\n\nAvailable {kind} profiles:\n{inventory}")
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn delegation_parameters(
    target_field: &str,
    target_kind: &str,
    targets: &[DelegationTarget],
    requires_confirmation: bool,
) -> serde_json::Value {
    let target_ids = targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<Vec<_>>();
    let mut properties = serde_json::Map::new();
    properties.insert(
        target_field.to_string(),
        serde_json::json!({
            "type": "string",
            "description": format!("Configured {target_kind} profile id"),
            "enum": target_ids
        }),
    );
    properties.insert(
        "task".to_string(),
        serde_json::json!({
            "type": "string",
            "description": "Focused task for the delegated child operation"
        }),
    );
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": [target_field, "task"],
        "additionalProperties": false
    });
    if requires_confirmation {
        schema["x-evo-authorization-risk"] = serde_json::json!("side_effect");
    }
    schema
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

#[cfg(test)]
mod tests {
    use std::fs;

    use agent_core::api::tool::AgentToolOutput;
    use ai::api::conversation::ContentBlock;
    use tempfile::tempdir;

    use super::*;
    use crate::operations::prompt::context::DelegationRequest;
    use crate::profiles::{DelegationConfirmationMode, ProfileKind, ProfileRegistryOptions};

    #[tokio::test]
    async fn delegate_agent_tool_accepts_allowed_request() {
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            allowed_agents: vec![ProfileId::from("coder")],
            ..DelegationPolicy::default()
        };
        let registry = test_registry();
        let inventory = DelegationTargetInventory::from_registry(&registry, &policy);
        let tools = delegation_tools(
            Some(&ProfileId::from("planner")),
            Some(&policy),
            &inventory,
            None,
        );

        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["delegate_agent"]
        );
        assert!(tools[0].parameters["properties"].get("agent_id").is_some());

        let response = run_tool(
            &tools[0],
            serde_json::json!({"agent_id": "coder", "task": "implement it"}),
        )
        .await;
        assert_eq!(response["status"], "requested");
        assert_eq!(response["target_kind"], "agent");
        assert_eq!(response["target_id"], "coder");
        assert_eq!(response["requesting_profile_id"], "planner");
    }

    #[tokio::test]
    async fn delegate_agent_tool_rejects_disallowed_target() {
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            allowed_agents: vec![ProfileId::from("coder")],
            ..DelegationPolicy::default()
        };
        let registry = test_registry();
        let inventory = DelegationTargetInventory::from_registry(&registry, &policy);
        let tools = delegation_tools(
            Some(&ProfileId::from("planner")),
            Some(&policy),
            &inventory,
            None,
        );

        let response = run_tool(
            &tools[0],
            serde_json::json!({"agent_id": "reviewer", "task": "review it"}),
        )
        .await;
        assert_eq!(response["status"], "rejected");
        assert!(
            response["message"]
                .as_str()
                .unwrap()
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn delegate_team_tool_rejects_zero_depth_policy() {
        let policy = DelegationPolicy {
            allow_delegate_team: true,
            max_depth: 0,
            allowed_teams: vec![ProfileId::from("implementation")],
            ..DelegationPolicy::default()
        };
        let registry = test_registry();
        let inventory = DelegationTargetInventory::from_registry(&registry, &policy);
        let tools = delegation_tools(
            Some(&ProfileId::from("planner")),
            Some(&policy),
            &inventory,
            None,
        );

        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["delegate_team"]
        );
        assert!(tools[0].parameters["properties"].get("team_id").is_some());
        let response = run_tool(
            &tools[0],
            serde_json::json!({"team_id": "implementation", "task": "build it"}),
        )
        .await;
        assert_eq!(response["status"], "rejected");
        assert!(response["message"].as_str().unwrap().contains("max_depth"));
    }

    #[test]
    fn built_in_agent_inventory_is_projected_into_schema_and_description() {
        let registry = ProfileRegistry::load(ProfileRegistryOptions::new()).unwrap();
        let profile = registry.agent("default").unwrap();
        let inventory = DelegationTargetInventory::from_registry(&registry, &profile.delegation);

        let tools = delegation_tools(
            Some(&profile.id),
            Some(&profile.delegation),
            &inventory,
            None,
        );

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "delegate_agent");
        assert_eq!(
            tools[0].parameters["properties"]["agent_id"]["enum"],
            serde_json::json!(["check", "explore", "review"])
        );
        for expected in [
            "check: Check - Read-only helper for safe verification planning and diagnostics",
            "explore: Explore - Read-only helper for context gathering and codebase exploration",
            "review: Review - Read-only helper for review and risk analysis",
        ] {
            assert!(
                tools[0].description.contains(expected),
                "missing inventory entry {expected:?}: {}",
                tools[0].description
            );
        }
    }

    #[test]
    fn inventory_filters_missing_allowlist_ids_and_expands_empty_allowlists() {
        let registry = test_registry();
        let filtered_policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            allowed_agents: vec![ProfileId::from("coder"), ProfileId::from("missing")],
            ..DelegationPolicy::default()
        };
        let filtered_inventory =
            DelegationTargetInventory::from_registry(&registry, &filtered_policy);
        let filtered_tools = delegation_tools(
            Some(&ProfileId::from("planner")),
            Some(&filtered_policy),
            &filtered_inventory,
            None,
        );
        assert_eq!(
            filtered_tools[0].parameters["properties"]["agent_id"]["enum"],
            serde_json::json!(["coder"])
        );

        let all_policy = DelegationPolicy {
            allow_delegate_agent: true,
            allow_delegate_team: true,
            max_depth: 1,
            ..DelegationPolicy::default()
        };
        let all_inventory = DelegationTargetInventory::from_registry(&registry, &all_policy);
        let all_tools = delegation_tools(
            Some(&ProfileId::from("planner")),
            Some(&all_policy),
            &all_inventory,
            None,
        );
        assert_eq!(
            all_tools[0].parameters["properties"]["agent_id"]["enum"],
            serde_json::json!(["check", "coder", "default", "explore", "review", "reviewer"])
        );
        assert_eq!(
            all_tools[1].parameters["properties"]["team_id"]["enum"],
            serde_json::json!(["implementation"])
        );
    }

    #[test]
    fn delegation_tool_is_not_exposed_without_an_effective_target() {
        let registry = test_registry();
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            allowed_agents: vec![ProfileId::from("missing")],
            ..DelegationPolicy::default()
        };
        let inventory = DelegationTargetInventory::from_registry(&registry, &policy);

        assert!(
            delegation_tools(
                Some(&ProfileId::from("planner")),
                Some(&policy),
                &inventory,
                None,
            )
            .is_empty()
        );
    }

    #[test]
    fn delegation_authorization_auto_approves_agent_when_confirmation_is_never() {
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            max_parallel_children: 2,
            require_confirmation: DelegationConfirmationMode::Never,
            allowed_agents: vec![ProfileId::from("coder")],
            ..DelegationPolicy::default()
        };
        let requests = vec![request("tool_1", ProfileKind::Agent, "coder")];

        let decisions = authorize_delegation_requests(&requests, &policy, 0);

        assert_eq!(decisions.len(), 1);
        assert!(matches!(
            &decisions[0],
            DelegationAuthorizationDecision::Approved {
                request,
                child_delegation_depth,
            }
                if request.tool_call_id == "tool_1"
                    && request.target_id.as_str() == "coder"
                    && *child_delegation_depth == 1
        ));
    }

    #[test]
    fn delegation_authorization_holds_team_when_writes_confirmation_required() {
        let policy = DelegationPolicy {
            allow_delegate_team: true,
            max_depth: 1,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Writes,
            allowed_teams: vec![ProfileId::from("implementation")],
            ..DelegationPolicy::default()
        };
        let requests = vec![request("tool_1", ProfileKind::Team, "implementation")];

        let decisions = authorize_delegation_requests(&requests, &policy, 0);

        assert_eq!(decisions.len(), 1);
        assert!(matches!(
            &decisions[0],
            DelegationAuthorizationDecision::RequiresConfirmation {
                request,
                reason,
                child_delegation_depth,
            }
                if request.target_id.as_str() == "implementation"
                    && reason.contains("team")
                    && *child_delegation_depth == 1
        ));
    }

    #[test]
    fn delegation_authorization_rejects_requests_past_child_limit() {
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Never,
            ..DelegationPolicy::default()
        };
        let requests = vec![
            request("tool_1", ProfileKind::Agent, "coder"),
            request("tool_2", ProfileKind::Agent, "reviewer"),
        ];

        let decisions = authorize_delegation_requests(&requests, &policy, 0);

        assert_eq!(decisions.len(), 2);
        assert!(matches!(
            &decisions[0],
            DelegationAuthorizationDecision::Approved { request, .. }
                if request.tool_call_id == "tool_1"
        ));
        assert!(matches!(
            &decisions[1],
            DelegationAuthorizationDecision::Rejected { request, reason }
                if request.tool_call_id == "tool_2" && reason.contains("max_parallel_children")
        ));
    }

    #[test]
    fn delegation_authorization_rejects_when_depth_budget_is_exhausted() {
        let policy = DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Never,
            ..DelegationPolicy::default()
        };
        let requests = vec![request("tool_1", ProfileKind::Agent, "coder")];

        let decisions = authorize_delegation_requests(&requests, &policy, 1);

        assert_eq!(decisions.len(), 1);
        assert!(matches!(
            &decisions[0],
            DelegationAuthorizationDecision::Rejected { request, reason }
                if request.tool_call_id == "tool_1" && reason.contains("max_depth")
        ));
    }

    fn request(tool_call_id: &str, target_kind: ProfileKind, target_id: &str) -> DelegationRequest {
        DelegationRequest {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: tool_call_id.into(),
            requesting_profile_id: ProfileId::from("planner"),
            target_kind,
            target_id: ProfileId::from(target_id),
            task: "help with task".into(),
        }
    }

    async fn run_tool(tool: &AgentTool, args: serde_json::Value) -> serde_json::Value {
        let output = (tool.execute)(
            agent_core::api::tool::ToolExecutionContext::standalone(tool.name.clone()),
            args,
            Some(std::sync::Arc::new(|_update: AgentToolOutput| {})),
        )
        .await
        .expect("tool should return structured text");
        let text = output
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .expect("text output");
        serde_json::from_str(text).unwrap()
    }

    fn test_registry() -> ProfileRegistry {
        let root = tempdir().unwrap();
        write_profile(
            &root.path().join("agents/coder.toml"),
            r#"
schema_version = 1
id = "coder"
display_name = "Coder"
description = "Implementation agent"
"#,
        );
        write_profile(
            &root.path().join("agents/reviewer.toml"),
            r#"
schema_version = 1
id = "reviewer"
display_name = "Reviewer"
description = "Review agent"
"#,
        );
        write_profile(
            &root.path().join("teams/implementation.toml"),
            r#"
schema_version = 1
id = "implementation"
display_name = "Implementation Team"
description = "Coder and reviewer"
supervisor = "deterministic"
members = ["coder", "reviewer"]
"#,
        );
        ProfileRegistry::load(ProfileRegistryOptions::new().with_project_root(root.path())).unwrap()
    }

    fn write_profile(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}
