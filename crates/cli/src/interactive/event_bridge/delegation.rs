use super::UiEvent;
use coding_agent::api::view::ProfileKind;

pub(super) fn delegation_block_from_tool_start(
    tool_call_id: &str,
    tool_name: &str,
    arguments_json: &str,
) -> Option<UiEvent> {
    let target_kind = delegation_tool_kind_label(tool_name)?;
    let args = parse_tool_arguments(arguments_json);
    let target_id_key = delegation_tool_target_key(tool_name)?;
    Some(UiEvent::DelegationBlock {
        call_id: tool_call_id.to_string(),
        target_kind: target_kind.to_string(),
        target_id: args
            .get(target_id_key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        task: args
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        status: "requested".to_string(),
        child_operation_id: None,
        summary: None,
        is_error: false,
    })
}

pub(super) fn delegation_block_from_tool_result(
    tool_call_id: &str,
    tool_name: &str,
    summary: &str,
) -> Option<UiEvent> {
    let fallback_kind = delegation_tool_kind_label(tool_name)?;
    let value: serde_json::Value = serde_json::from_str(summary).ok()?;
    let status = value
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("requested");
    let message = value
        .get("message")
        .or_else(|| value.get("error"))
        .and_then(|value| value.as_str())
        .unwrap_or(status);
    let is_error = matches!(status, "rejected" | "failed" | "cancelled");
    let summary = match status {
        "requested" => Some("requested".to_string()),
        "rejected" => Some(format!("rejected: {message}")),
        "failed" => Some(format!("failed: {message}")),
        "cancelled" => Some(format!("cancelled: {message}")),
        other => Some(other.to_string()),
    };
    Some(UiEvent::DelegationBlock {
        call_id: tool_call_id.to_string(),
        target_kind: value
            .get("target_kind")
            .and_then(|value| value.as_str())
            .unwrap_or(fallback_kind)
            .to_string(),
        target_id: value
            .get("target_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        task: value
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        status: status.to_string(),
        child_operation_id: None,
        summary,
        is_error,
    })
}

pub(super) fn is_delegation_tool(name: &str) -> bool {
    delegation_tool_kind_label(name).is_some()
}

pub(super) fn delegation_tool_kind_label(name: &str) -> Option<&'static str> {
    match name {
        "delegate_agent" => Some("agent"),
        "delegate_team" => Some("team"),
        _ => None,
    }
}

fn delegation_tool_target_key(name: &str) -> Option<&'static str> {
    match name {
        "delegate_agent" => Some("agent_id"),
        "delegate_team" => Some("team_id"),
        _ => None,
    }
}

pub(super) fn parse_tool_arguments(arguments_json: &str) -> serde_json::Value {
    serde_json::from_str(arguments_json)
        .unwrap_or_else(|_| serde_json::Value::String(arguments_json.to_string()))
}

pub(super) fn profile_kind_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}
