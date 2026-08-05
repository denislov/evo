use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};
use tool_contract::api::output::ToolErrorKind;

use crate::agent::tool_adapter::{error_result, serialized_output_bytes};
use crate::agent::turn::context::{AgentTurnContext, PendingToolCall};
use crate::agent::turn::nodes::ToolExecutionLimit;
use crate::agent::turn::tools::{ExecutableTool, execute_executable_tool};
use crate::agent::types::{
    AgentEvent, AgentToolOutput, AgentToolResult, ToolExecutionContext, ToolUpdateCallback,
};

const TOOL_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const TOOL_TEARDOWN_GRACE: Duration = Duration::from_secs(5);
const MAX_TOOL_UPDATES_PER_CALL: usize = 64;
const MAX_TOOL_UPDATE_BYTES_PER_CALL: usize = 256 * 1024;

pub(super) async fn execute_tool_with_updates(
    ctx: &mut AgentTurnContext,
    call: &PendingToolCall,
    tool: Option<ExecutableTool>,
) -> AgentToolResult {
    let cooperative_teardown = supports_cooperative_teardown(tool.as_ref());
    let (update_tx, mut update_rx) = mpsc::unbounded::<AgentToolOutput>();
    let progress_budget = Arc::new(std::sync::Mutex::new(ToolProgressBudget::default()));
    let callback_budget = Arc::clone(&progress_budget);
    let update_callback: ToolUpdateCallback = Arc::new(move |update| {
        let Some(bytes) = serialized_output_bytes(&update) else {
            callback_budget.lock().unwrap().exceeded = true;
            return;
        };
        let mut budget = callback_budget.lock().unwrap();
        let next_events = budget.events.saturating_add(1);
        let next_bytes = budget.bytes.saturating_add(bytes);
        if next_events > MAX_TOOL_UPDATES_PER_CALL || next_bytes > MAX_TOOL_UPDATE_BYTES_PER_CALL {
            budget.exceeded = true;
            return;
        }
        budget.events = next_events;
        budget.bytes = next_bytes;
        let _ = update_tx.unbounded_send(update);
    });
    let execution_cancel = ctx.cancel_token.child_token();
    let tool_cancel = execution_cancel.clone();
    let mut execute_future = Box::pin({
        let arguments = call.arguments.clone();
        let execution_context = ToolExecutionContext::new(
            ctx.config.tool_execution_scope.clone(),
            ctx.turn,
            call.id.clone(),
            call.name.clone(),
            tool_cancel,
        );
        execute_executable_tool(
            tool,
            execution_context,
            arguments,
            Some(update_callback),
            Instant::now() + TOOL_EXECUTION_TIMEOUT,
        )
    })
    .fuse();
    let mut deadline = Box::pin(tokio::time::sleep(TOOL_EXECUTION_TIMEOUT).fuse());
    let mut update_open = true;
    let cancellation_token = ctx.cancel_token.clone();
    let result = loop {
        if !update_open {
            break tokio::select! {
                _ = cancellation_token.clone().cancelled_owned() => {
                    execution_cancel.cancel();
                    wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
                    error_result(ToolErrorKind::Cancelled, "aborted")
                }
                _ = &mut deadline => {
                    execution_cancel.cancel();
                    wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
                    error_result(
                        ToolErrorKind::Timeout,
                        ToolExecutionLimit::Deadline.message(),
                    )
                }
                result = &mut execute_future => result,
            };
        }
        tokio::select! {
            _ = cancellation_token.clone().cancelled_owned() => {
                execution_cancel.cancel();
                wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
                break error_result(ToolErrorKind::Cancelled, "aborted");
            }
            _ = &mut deadline => {
                execution_cancel.cancel();
                wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
                break error_result(
                    ToolErrorKind::Timeout,
                    ToolExecutionLimit::Deadline.message(),
                );
            }
            maybe_update = update_rx.next().fuse() => {
                if let Some(update) = maybe_update {
                    ctx.emit(AgentEvent::ToolCallUpdate {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        update,
                    });
                } else {
                    update_open = false;
                }
            }
            completed = &mut execute_future => break completed,
        }
    };
    while let Some(Some(update)) = update_rx.next().now_or_never() {
        ctx.emit(AgentEvent::ToolCallUpdate {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            update,
        });
    }
    if progress_budget.lock().unwrap().exceeded {
        error_result(
            ToolErrorKind::Protocol,
            ToolExecutionLimit::Progress.message(),
        )
    } else {
        result
    }
}

pub(super) async fn execute_tool(
    tool: Option<ExecutableTool>,
    execution_context: ToolExecutionContext,
    arguments: serde_json::Value,
) -> AgentToolResult {
    let cooperative_teardown = supports_cooperative_teardown(tool.as_ref());
    let cancellation = execution_context.cancel_token().clone();
    let mut execute_future = Box::pin(execute_executable_tool(
        tool,
        execution_context,
        arguments,
        None,
        Instant::now() + TOOL_EXECUTION_TIMEOUT,
    ));
    tokio::select! {
        _ = cancellation.clone().cancelled_owned() => {
            cancellation.cancel();
            wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
            error_result(ToolErrorKind::Cancelled, "aborted")
        }
        _ = tokio::time::sleep(TOOL_EXECUTION_TIMEOUT) => {
            cancellation.cancel();
            wait_for_tool_teardown(&mut execute_future, cooperative_teardown).await;
            error_result(
                ToolErrorKind::Timeout,
                ToolExecutionLimit::Deadline.message(),
            )
        }
        result = &mut execute_future => result,
    }
}

fn supports_cooperative_teardown(tool: Option<&ExecutableTool>) -> bool {
    matches!(
        tool,
        Some(ExecutableTool::Runtime { definition, .. }) if definition.capabilities.cancel
    )
}

async fn wait_for_tool_teardown<F>(future: &mut F, cooperative: bool)
where
    F: std::future::Future + Unpin,
{
    if cooperative {
        let _ = tokio::time::timeout(TOOL_TEARDOWN_GRACE, future).await;
    }
}

#[derive(Default)]
struct ToolProgressBudget {
    events: usize,
    bytes: usize,
    exceeded: bool,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::{
        AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition,
        ToolExecutionMode, ToolId, ToolKind,
    };
    use tool_contract::api::output::{ToolError, ToolErrorKind};
    use tool_contract::api::schema::schema_for;
    use tool_runtime::api::{ToolRegistry, ToolRuntime, TypedTool};

    use super::*;

    #[derive(Deserialize, JsonSchema)]
    struct WaitArgs {
        value: String,
    }

    #[tokio::test]
    async fn outer_turn_executor_waits_for_typed_runtime_teardown() {
        let started = Arc::new(Notify::new());
        let cleaned = Arc::new(AtomicBool::new(false));
        let definition = ToolDefinition {
            id: ToolId::new("wait").unwrap(),
            kind: ToolKind::Function,
            description: "Wait until cancelled".into(),
            parameters: schema_for::<WaitArgs>().unwrap(),
            capabilities: ToolCapabilities {
                read_only: false,
                execution: ToolExecutionMode::Parallel,
                cancel: true,
                timeout: true,
                streaming: false,
                provider_executed: false,
            },
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        };
        let tool = TypedTool::<WaitArgs>::new(definition.clone(), {
            let started = started.clone();
            let cleaned = cleaned.clone();
            move |context, args| {
                let started = started.clone();
                let cleaned = cleaned.clone();
                Box::pin(async move {
                    assert_eq!(args.value, "probe");
                    started.notify_one();
                    context.cancel.cancelled().await;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    cleaned.store(true, Ordering::Release);
                    Err(ToolError::new(ToolErrorKind::Cancelled, "cleaned"))
                })
            }
        })
        .unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(tool)).unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let executable = ExecutableTool::Runtime {
            runtime,
            definition,
        };
        let cancellation = CancellationToken::new();
        let task_cancel = cancellation.clone();
        let task = tokio::spawn(async move {
            execute_tool(
                Some(executable),
                ToolExecutionContext::new(None::<String>, 1, "wait-call", "wait", task_cancel),
                serde_json::json!({"value": "probe"}),
            )
            .await
        });
        started.notified().await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("outer executor should wait for bounded teardown")
            .unwrap();
        assert!(result.is_error);
        assert!(cleaned.load(Ordering::Acquire));
    }
}
