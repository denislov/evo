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

#[cfg(test)]
mod tests {
    use crate::agent::types::{
        AgentMessage, AgentTool, AgentToolOutput, AgentToolResult, ToolExecutionMode,
    };
    use ai::api::conversation::{AssistantMessage, ContentBlock};
    use std::sync::Arc;

    fn text_tool(name: &str, execution_mode: Option<ToolExecutionMode>) -> AgentTool {
        AgentTool {
            name: name.into(),
            description: "test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode,
            execute: Arc::new(|_, _, _| Box::pin(async { Ok(AgentToolOutput::new(vec![])) })),
        }
    }

    #[test]
    fn extract_tool_calls_preserves_assistant_order() {
        let mut assistant = AssistantMessage::empty("test", "test-model");
        assistant.content = vec![
            ContentBlock::Text {
                text: "before".into(),
                text_signature: None,
            },
            ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "first".into(),
                arguments: serde_json::json!({"n": 1}),
                thought_signature: None,
            },
            ContentBlock::ToolCall {
                id: "call_2".into(),
                name: "second".into(),
                arguments: serde_json::json!({"n": 2}),
                thought_signature: None,
            },
        ];

        let calls = super::extract_tool_calls(&assistant);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].index, 1);
        assert_eq!(calls[0].tool_call_id, "call_1");
        assert_eq!(calls[0].tool_name, "first");
        assert_eq!(calls[1].index, 2);
        assert_eq!(calls[1].tool_call_id, "call_2");
        assert_eq!(calls[1].tool_name, "second");
    }

    #[test]
    fn should_use_sequential_tools_honors_global_and_per_tool_modes() {
        let calls = vec![super::ToolCallRequest {
            index: 0,
            tool_call_id: "call_1".into(),
            tool_name: "serial".into(),
            arguments: serde_json::json!({}),
        }];
        let tools = vec![text_tool("serial", Some(ToolExecutionMode::Sequential))];

        assert!(super::should_use_sequential_tools(
            ToolExecutionMode::Sequential,
            &calls,
            &[]
        ));
        assert!(super::should_use_sequential_tools(
            ToolExecutionMode::Parallel,
            &calls,
            &tools
        ));
        assert!(!super::should_use_sequential_tools(
            ToolExecutionMode::Parallel,
            &calls,
            &[text_tool("serial", None)]
        ));
    }

    #[test]
    fn append_tool_result_messages_preserves_current_message_shape() {
        let mut messages = Vec::new();
        let executions = vec![super::ToolCallExecution {
            index: 0,
            tool_call_id: "call_1".into(),
            tool_name: "echo".into(),
            result: AgentToolResult::error("failed"),
        }];

        super::append_tool_result_messages(&mut messages, &executions);

        assert!(matches!(
            &messages[0],
            AgentMessage::ToolResult {
                message_id,
                tool_call_id,
                tool_name,
                is_error: true,
                content,
            } if message_id == "call_1"
                && tool_call_id == "call_1"
                && tool_name == "echo"
                && matches!(&content[0], ContentBlock::Text { text, .. } if text == "failed")
        ));
    }
}
