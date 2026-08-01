use crate::error::CliError;
use crate::protocol::jsonl::serialize_json_line;
use crate::protocol::types::RpcResponse;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub async fn write_rpc_response<W>(writer: &mut W, response: RpcResponse) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    write_json_line(writer, &response).await
}

pub(super) async fn write_json_line<W, T>(writer: &mut W, value: &T) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let line = serialize_json_line(value).map_err(|e| CliError::AgentFailure(e.to_string()))?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| CliError::AgentFailure(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| CliError::AgentFailure(e.to_string()))
}

pub(super) fn command_type(value: &Value) -> String {
    value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

pub(super) fn command_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

pub(super) fn is_supported_m5_command(command: &str) -> bool {
    matches!(
        command,
        "hello"
            | "detach"
            | "shutdown"
            | "prompt"
            | "steer"
            | "follow_up"
            | "abort"
            | "new_session"
            | "get_state"
            | "reload"
            | "self_healing_edit"
            | "list_agent_profiles"
            | "list_tool_authorizations"
            | "approve_tool_authorization"
            | "deny_tool_authorization"
            | "list_team_profiles"
            | "invoke_agent"
            | "invoke_team"
            | "list_delegation_confirmations"
            | "approve_delegation"
            | "reject_delegation"
            | "set_thinking_level"
            | "set_steering_mode"
            | "set_follow_up_mode"
            | "compact"
            | "set_auto_compaction"
            | "get_session_stats"
            | "get_last_assistant_text"
            | "set_session_name"
            | "get_messages"
    )
}
