use crate::agent::types::{AgentMessage, AgentTool, AgentToolResult, ToolExecutionMode};
use ai::api::conversation::{AssistantMessage, ContentBlock};

pub(crate) struct ToolCallRequest {
    pub index: usize,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

pub(crate) struct ToolCallExecution {
    pub index: usize,
    pub tool_call_id: String,
    pub tool_name: String,
    pub result: AgentToolResult,
}

pub(crate) fn extract_tool_calls(assistant: &AssistantMessage) -> Vec<ToolCallRequest> {
    assistant
        .content
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(ToolCallRequest {
                index,
                tool_call_id: id.clone(),
                tool_name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

pub(crate) fn should_use_sequential_tools(
    global_mode: ToolExecutionMode,
    calls: &[ToolCallRequest],
    tools: &[AgentTool],
) -> bool {
    global_mode == ToolExecutionMode::Sequential
        || calls.iter().any(|call| {
            tools
                .iter()
                .find(|tool| tool.name == call.tool_name)
                .and_then(|tool| tool.execution_mode)
                == Some(ToolExecutionMode::Sequential)
        })
}

pub(crate) fn append_tool_result_messages(
    messages: &mut Vec<AgentMessage>,
    executions: &[ToolCallExecution],
) {
    let mut ordered: Vec<_> = executions.iter().collect();
    ordered.sort_by_key(|execution| execution.index);
    for execution in ordered {
        messages.push(AgentMessage::ToolResult {
            message_id: execution.tool_call_id.clone(),
            tool_call_id: execution.tool_call_id.clone(),
            tool_name: execution.tool_name.clone(),
            is_error: execution.result.is_error,
            content: execution.result.content.clone(),
        });
    }
}
