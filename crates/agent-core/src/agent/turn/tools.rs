use std::time::Instant;

use crate::agent::tool_adapter::error_result;
use crate::agent::types::{
    AgentMessage, AgentToolOutput, AgentToolResult, ToolExecutionContext, ToolUpdateCallback,
};
use ai_protocol::api::conversation::{AssistantMessage, ContentBlock};
use tool_contract::api::definition::{ToolDefinition, ToolExecutionMode, ToolId};
use tool_contract::api::output::ToolError;
use tool_contract::api::output::ToolErrorKind;
use tool_runtime::api::{ProgressSink, ToolCallContext, ToolRuntime};

#[derive(Clone)]
pub(crate) enum ExecutableTool {
    Runtime {
        runtime: ToolRuntime,
        definition: ToolDefinition,
    },
}

impl ExecutableTool {
    pub(crate) fn validate_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<(), ToolError> {
        match self {
            Self::Runtime {
                runtime,
                definition,
            } => runtime.validate_arguments(&definition.id, arguments),
        }
    }
}

pub(crate) fn find_executable_tool(
    runtime: Option<&ToolRuntime>,
    name: &str,
) -> Option<ExecutableTool> {
    let runtime = runtime?;
    let id = ToolId::new(name).ok()?;
    runtime
        .definition(&id)
        .map(|definition| ExecutableTool::Runtime {
            runtime: runtime.clone(),
            definition,
        })
}

pub(crate) async fn execute_executable_tool(
    tool: Option<ExecutableTool>,
    context: ToolExecutionContext,
    arguments: serde_json::Value,
    update: Option<ToolUpdateCallback>,
    deadline: Instant,
) -> AgentToolResult {
    let tool_name = context.tool_name().to_owned();
    match tool {
        Some(ExecutableTool::Runtime {
            runtime,
            definition,
        }) => {
            let progress = update.map(|callback| {
                ProgressSink::new(move |progress| callback(AgentToolOutput::from(progress)))
            });
            let runtime_context = ToolCallContext::new(
                definition.id,
                context.tool_call_id(),
                context.cancel_token().clone(),
            )
            .with_operation_id(context.scope_id().map(str::to_owned))
            .with_turn(context.turn())
            .with_deadline(Some(deadline))
            .with_progress(progress);
            match runtime.execute(runtime_context, arguments).await {
                Ok(output) => output.into(),
                Err(error) => error.into(),
            }
        }
        None => error_result(
            ToolErrorKind::Unavailable,
            format!("unknown tool: {tool_name}"),
        ),
    }
}

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
    runtime_tools: &[ToolDefinition],
) -> bool {
    global_mode == ToolExecutionMode::Sequential
        || calls.iter().any(|call| {
            runtime_tools.iter().any(|tool| {
                tool.id.as_str() == call.tool_name
                    && tool.capabilities.execution == ToolExecutionMode::Sequential
            })
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
