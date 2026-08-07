use super::*;

pub(in crate::rpc) fn rpc_self_healing_edit_replacement(
    edit: RpcSelfHealingEditReplacement,
) -> SelfHealingEditReplacement {
    SelfHealingEditReplacement::new(edit.old_text, edit.new_text)
}

pub(in crate::rpc) fn rpc_self_healing_edit_data(
    outcome: &SelfHealingEditOutcome,
) -> serde_json::Value {
    serde_json::json!({
        "path": outcome.path,
        "message": outcome.message,
        "diff": outcome.diff,
        "patch": outcome.patch,
        "firstChangedLine": outcome.first_changed_line,
        "attempts": outcome.attempts,
        "diagnostics": outcome
            .diagnostics
            .iter()
            .map(|diagnostic| serde_json::json!({ "message": diagnostic.message }))
            .collect::<Vec<_>>(),
        "checkOutput": outcome
            .check_output
            .as_ref()
            .map(rpc_self_healing_check_output_data),
        "repairAttempts": outcome
            .repair_attempts
            .iter()
            .map(rpc_self_healing_repair_attempt_data)
            .collect::<Vec<_>>(),
    })
}

pub(in crate::rpc) fn rpc_self_healing_repair_attempt_data(
    repair: &SelfHealingEditRepairAttempt,
) -> serde_json::Value {
    serde_json::json!({
        "attempt": repair.attempt,
        "edits": repair
            .replacements
            .iter()
            .map(|replacement| serde_json::json!({
                "oldText": replacement.old_text,
                "newText": replacement.new_text,
            }))
            .collect::<Vec<_>>(),
        "diagnostics": repair
            .diagnostics
            .iter()
            .map(|diagnostic| serde_json::json!({ "message": diagnostic.message }))
            .collect::<Vec<_>>(),
        "checkOutput": repair
            .check_output
            .as_ref()
            .map(rpc_self_healing_check_output_data),
    })
}

pub(in crate::rpc) fn rpc_self_healing_check_output_data(
    output: &SelfHealingEditCheckOutput,
) -> serde_json::Value {
    serde_json::json!({
        "command": output.command,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "exitCode": output.exit_code,
    })
}

pub(in crate::rpc) fn rpc_agent_profiles_data(
    session: &CodingAgentSession,
) -> Result<serde_json::Value, CliError> {
    let view = session.view()?;
    let default_profile_id = view.default_agent_profile_id;
    let agents = session
        .agent_profiles()
        .into_iter()
        .map(|profile| rpc_agent_profile(&profile))
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "defaultAgentProfileId": default_profile_id.as_str(),
        "agents": agents,
        "diagnostics": rpc_profile_diagnostics(session),
    }))
}

pub(in crate::rpc) fn rpc_team_profiles_data(session: &CodingAgentSession) -> serde_json::Value {
    let teams = session
        .team_profiles()
        .into_iter()
        .map(|profile| rpc_team_profile(&profile))
        .collect::<Vec<_>>();

    serde_json::json!({
        "teams": teams,
        "diagnostics": rpc_profile_diagnostics(session),
    })
}

pub(in crate::rpc) fn rpc_agent_profile(
    profile: &CodingAgentAgentProfileSummary,
) -> serde_json::Value {
    serde_json::json!({
        "id": profile.id.as_str(),
        "displayName": profile.display_name,
        "description": profile.description.as_deref(),
        "source": rpc_profile_source(profile.source),
        "isDefault": profile.is_default,
        "model": profile.model_id.as_deref(),
        "tools": profile.tools,
        "skills": profile.skills,
        "supervision": rpc_supervision_policy(&profile.supervision),
        "delegation": rpc_delegation_policy(&profile.delegation),
    })
}

pub(in crate::rpc) fn rpc_team_profile(
    profile: &CodingAgentTeamProfileSummary,
) -> serde_json::Value {
    serde_json::json!({
        "id": profile.id.as_str(),
        "displayName": profile.display_name,
        "description": profile.description.as_deref(),
        "source": rpc_profile_source(profile.source),
        "supervisor": rpc_team_supervisor(&profile.supervisor),
        "strategy": rpc_team_strategy(&profile.strategy),
        "members": rpc_profile_id_list(&profile.members),
        "delegation": rpc_delegation_policy(&profile.delegation),
    })
}

pub(in crate::rpc) fn rpc_pending_delegation_confirmation(
    pending: &PendingDelegationConfirmation,
) -> serde_json::Value {
    serde_json::json!({
        "operationId": pending.operation_id,
        "turnId": pending.turn_id,
        "toolCallId": pending.tool_call_id,
        "requestingProfileId": pending.requesting_profile_id.as_str(),
        "targetKind": rpc_profile_kind(pending.target_kind),
        "targetId": pending.target_id.as_str(),
        "task": pending.task,
        "reason": pending.reason,
    })
}

pub(in crate::rpc) fn rpc_profile_diagnostics(
    session: &CodingAgentSession,
) -> Vec<serde_json::Value> {
    session
        .profile_diagnostics()
        .into_iter()
        .map(|diagnostic| rpc_profile_diagnostic(&diagnostic))
        .collect()
}

pub(in crate::rpc) fn rpc_profile_diagnostic(
    diagnostic: &CodingAgentPublicDiagnostic,
) -> serde_json::Value {
    serde_json::json!({
        "severity": diagnostic.severity,
        "code": diagnostic.code,
        "summary": diagnostic.summary,
        "origin": diagnostic.origin,
        "operationId": diagnostic.operation_id,
    })
}

pub(in crate::rpc) fn rpc_delegation_policy(policy: &DelegationPolicy) -> serde_json::Value {
    serde_json::json!({
        "allowDelegateAgent": policy.allow_delegate_agent,
        "allowDelegateTeam": policy.allow_delegate_team,
        "maxDepth": policy.max_depth,
        "maxParallelChildren": policy.max_parallel_children,
        "requireConfirmation": rpc_delegation_confirmation_mode(&policy.require_confirmation),
        "allowedAgents": rpc_profile_id_list(&policy.allowed_agents),
        "allowedTeams": rpc_profile_id_list(&policy.allowed_teams),
    })
}

pub(in crate::rpc) fn rpc_profile_id_list(ids: &[ProfileId]) -> Vec<&str> {
    ids.iter().map(ProfileId::as_str).collect()
}

pub(in crate::rpc) fn rpc_team_supervisor(supervisor: &TeamSupervisor) -> serde_json::Value {
    match supervisor {
        TeamSupervisor::Deterministic => serde_json::json!({ "mode": "deterministic" }),
        TeamSupervisor::Agent(profile_id) => serde_json::json!({
            "mode": "agent",
            "profileId": profile_id.as_str(),
        }),
    }
}

pub(in crate::rpc) fn rpc_profile_source(source: ProfileSource) -> &'static str {
    match source {
        ProfileSource::BuiltIn => "built_in",
        ProfileSource::User => "user",
        ProfileSource::Project => "project",
    }
}

pub(in crate::rpc) fn rpc_profile_kind(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}

pub(in crate::rpc) fn rpc_supervision_policy(policy: &SupervisionPolicy) -> &'static str {
    match policy {
        SupervisionPolicy::Session => "session",
        SupervisionPolicy::SelfReview => "self_review",
        SupervisionPolicy::LlmSupervisor => "llm_supervisor",
    }
}

pub(in crate::rpc) fn rpc_delegation_confirmation_mode(
    mode: &DelegationConfirmationMode,
) -> &'static str {
    match mode {
        DelegationConfirmationMode::Never => "never",
        DelegationConfirmationMode::Writes => "writes",
        DelegationConfirmationMode::Always => "always",
    }
}

pub(in crate::rpc) fn rpc_team_strategy(strategy: &TeamStrategy) -> &'static str {
    match strategy {
        TeamStrategy::PlanExecuteReview => "plan_execute_review",
    }
}

pub(in crate::rpc) fn rpc_transcript_item(
    item: CodingAgentSessionTranscriptItem,
) -> serde_json::Value {
    match item {
        CodingAgentSessionTranscriptItem::User { text, started_at } => serde_json::json!({
            "role": "user",
            "content": text,
            "startedAt": started_at,
        }),
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
            reasoning_duration_millis,
            model_id,
            completed_at,
        } => serde_json::json!({
            "role": "assistant",
            "id": id,
            "content": text,
            "thinking": thinking,
            "images": images,
            "done": done,
            "reasoningDurationMillis": reasoning_duration_millis,
            "modelId": model_id,
            "completedAt": completed_at,
        }),
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
            duration_millis,
        } => serde_json::json!({
            "role": "tool",
            "callId": call_id,
            "name": name,
            "arguments": args,
            "result": result,
            "isError": is_error,
            "durationMillis": duration_millis,
        }),
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => serde_json::json!({
            "role": "delegation",
            "toolCallId": tool_call_id,
            "requestingProfileId": requesting_profile_id.as_str(),
            "targetKind": rpc_profile_kind(target_kind),
            "targetId": target_id.as_str(),
            "task": task,
            "status": status,
            "childOperationId": child_operation_id,
            "summary": summary,
        }),
        CodingAgentSessionTranscriptItem::CompactionSummary { summary } => {
            serde_json::json!({"role": "compactionSummary", "summary": summary})
        }
        CodingAgentSessionTranscriptItem::BranchSummary { summary } => {
            serde_json::json!({"role": "branchSummary", "summary": summary})
        }
        CodingAgentSessionTranscriptItem::Diagnostic { message } => {
            serde_json::json!({"role": "diagnostic", "message": message})
        }
    }
}

pub(in crate::rpc) fn has_images(images: &Option<Vec<CodingAgentPromptImage>>) -> bool {
    images.as_ref().is_some_and(|images| !images.is_empty())
}
