use std::collections::HashSet;
use std::sync::Arc;

use crate::agent::provider::stream_model_with_provider_streamer;
use crate::agent::queue::{QueueMode, drain_queue};
use crate::agent::tool_adapter::{error_result, serialized_result_bytes};
use crate::agent::turn::options::stream_options_for_turn;
use crate::agent::turn::tool_execution::{execute_tool, execute_tool_with_updates};
use crate::agent::turn::tools::{
    ExecutableTool, ToolCallExecution, ToolCallRequest, append_tool_result_messages,
    extract_tool_calls, find_executable_tool, should_use_sequential_tools,
};
use crate::agent::types::{
    AgentEvent, AgentMessage, AgentToolResult, ProviderRequestSnapshot, ToolExecutionContext,
};
use crate::compaction::estimate::estimate_context_tokens;
use crate::compaction::prepare::{prepare_compaction, should_compact};
use crate::compaction::summarize::summarize_with_provider_streamer;
use crate::context::conversion::{assemble_context, convert_to_context, default_convert_to_llm};
use crate::hooks::{
    AfterToolCallContext, AfterToolCallHook, BeforeProviderRequestContext, BeforeToolCallContext,
    PrepareNextTurnContext, ShouldStopAfterTurnContext,
};
use ai_protocol::api::conversation::{AssistantMessage, ContentBlock, StopReason, Usage};
use ai_protocol::api::stream::AssistantMessageEvent;
use ai_protocol::api::stream::json::parse_terminal_json;
use futures::{FutureExt, StreamExt};
use tokio_util::sync::CancellationToken;
use tool_contract::api::output::ToolErrorKind;

use super::context::{AgentTurnContext, PendingToolCall, RuntimeCompactionState};

const MAX_TOOL_CALLS_PER_TURN: usize = 64;
const MAX_CONCURRENT_TOOL_CALLS: usize = 8;
const MAX_TOOL_RESULT_BYTES_PER_CALL: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolExecutionLimit {
    CallsPerTurn,
    Deadline,
    Progress,
    Result,
}

impl ToolExecutionLimit {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::CallsPerTurn => "tool-call count exceeds the per-turn limit",
            Self::Deadline => "tool execution deadline exceeded",
            Self::Progress => "tool progress exceeds the retention limit",
            Self::Result => "tool result exceeds the retention limit",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum AgentTurnError {
    Invariant(String),
    Compaction(String),
    ToolLimit(ToolExecutionLimit),
}

impl std::fmt::Display for AgentTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invariant(msg) | Self::Compaction(msg) => write!(f, "{msg}"),
            Self::ToolLimit(limit) => write!(f, "{}", limit.message()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTurnDecision {
    Next,
    Continue,
    ContinueProvider,
    Tools,
    Done,
    Error,
    Aborted,
}

pub(crate) fn start_turn(ctx: &mut AgentTurnContext) -> Result<AgentTurnDecision, AgentTurnError> {
    if ctx.cancel_token.is_cancelled() {
        ctx.emit(AgentEvent::AgentError {
            error: "aborted".into(),
        });
        return Ok(AgentTurnDecision::Aborted);
    }

    ctx.turn = ctx
        .turn
        .checked_add(1)
        .ok_or_else(|| AgentTurnError::Invariant("agent turn counter overflowed".into()))?;
    if let Some(max_turns) = ctx.config.max_turns
        && ctx.turn > max_turns
    {
        ctx.emit(AgentEvent::AgentError {
            error: format!("max turns ({}) exceeded", max_turns),
        });
        return Ok(AgentTurnDecision::Error);
    }

    ctx.emit(AgentEvent::TurnStart { turn: ctx.turn });
    Ok(AgentTurnDecision::Next)
}

pub(crate) fn drain_queued_input(ctx: &mut AgentTurnContext) {
    let interjected = drain_queue(&mut ctx.interjection_queue, QueueMode::All);
    ctx.messages.extend(interjected);
    let steered = drain_queue(&mut ctx.steering_queue, ctx.config.steering_mode);
    ctx.messages.extend(steered);
}

pub(crate) async fn prepare_provider_request(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    let transformed_messages = if let Some(hook) = ctx.config.hooks.transform_context.clone() {
        let cancellation_token = ctx.cancel_token.clone();
        match tokio::select! {
            _ = cancellation_token.clone().cancelled_owned() => return aborted(ctx),
            result = hook(ctx.messages.clone()) => result,
        } {
            Ok(messages) => Some(messages),
            Err(error) => {
                ctx.emit(AgentEvent::AgentError {
                    error: error.clone(),
                });
                return Ok(AgentTurnDecision::Error);
            }
        }
    } else {
        None
    };

    let llm_messages_override = if let Some(hook) = ctx.config.hooks.convert_to_llm.clone() {
        let messages = transformed_messages
            .clone()
            .unwrap_or_else(|| ctx.messages.clone());
        let cancellation_token = ctx.cancel_token.clone();
        match tokio::select! {
            _ = cancellation_token.clone().cancelled_owned() => return aborted(ctx),
            result = hook(messages, ctx.config.resources.clone()) => result,
        } {
            Ok(llm_messages) => Some(llm_messages),
            Err(error) => {
                ctx.emit(AgentEvent::AgentError {
                    error: error.clone(),
                });
                return Ok(AgentTurnDecision::Error);
            }
        }
    } else {
        None
    };

    let messages_for_context = transformed_messages.as_ref().unwrap_or(&ctx.messages);
    let runtime_tools = ctx
        .tool_runtime
        .as_ref()
        .map(tool_runtime::api::ToolRuntime::definitions)
        .unwrap_or_default();
    let context = if let Some(llm_messages) = llm_messages_override {
        assemble_context(
            &ctx.config.system_prompt,
            messages_for_context,
            llm_messages,
            &runtime_tools,
            &ctx.provider_tools,
            &ctx.config.resources,
        )
    } else if transformed_messages.is_some() {
        let llm_messages = default_convert_to_llm(messages_for_context, &ctx.config.resources);
        assemble_context(
            &ctx.config.system_prompt,
            messages_for_context,
            llm_messages,
            &runtime_tools,
            &ctx.provider_tools,
            &ctx.config.resources,
        )
    } else {
        convert_to_context(
            &ctx.config.system_prompt,
            &ctx.messages,
            &runtime_tools,
            &ctx.provider_tools,
            &ctx.config.resources,
        )
    };

    let mut stream_options = stream_options_for_turn(
        &ctx.config.model,
        ctx.config.stream_options.clone().unwrap_or_default(),
        ctx.config.thinking_level,
    );
    stream_options.cancel = Some(ctx.cancel_token.clone());

    let mut request = ProviderRequestSnapshot {
        model: ctx.config.model.clone(),
        context,
        stream_options,
    };

    if let Some(override_request) = ctx.take_provider_request_override() {
        request.context = override_request.context;
        if let Some(override_options) = override_request.stream_options {
            request.stream_options = override_options;
        }
        request.stream_options.cancel = Some(ctx.cancel_token.clone());
    }

    ctx.provider_request = Some(request);

    Ok(AgentTurnDecision::Next)
}

pub(crate) async fn apply_before_provider_request_hook(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    let mut request = match ctx.provider_request.clone() {
        Some(request) => request,
        None => {
            let error = "provider request is not prepared".to_string();
            ctx.emit(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Ok(AgentTurnDecision::Error);
        }
    };

    if let Some(hook) = ctx.config.hooks.before_provider_request.clone() {
        let cancellation_token = ctx.cancel_token.clone();
        match tokio::select! {
            _ = cancellation_token.clone().cancelled_owned() => return aborted(ctx),
            result = hook(BeforeProviderRequestContext::from(request.clone())) => result,
        } {
            Ok(Some(update)) => {
                if let Some(updated_context) = update.context {
                    request.context = updated_context;
                }
                if let Some(updated_options) = update.stream_options {
                    request.stream_options = updated_options;
                }
                request.stream_options.cancel = Some(ctx.cancel_token.clone());
            }
            Ok(None) => {}
            Err(error) => {
                ctx.emit(AgentEvent::AgentError {
                    error: error.clone(),
                });
                return Ok(AgentTurnDecision::Error);
            }
        }
    }

    ctx.provider_request = Some(request.clone());
    ctx.emit(AgentEvent::BeforeProviderRequest { request });
    Ok(AgentTurnDecision::Next)
}

pub(crate) async fn maybe_compact_runtime_context(
    ctx: &mut AgentTurnContext,
) -> Result<(), AgentTurnError> {
    let Some(config) = ctx.config.compaction.clone() else {
        return Ok(());
    };

    let usage_estimate = estimate_context_tokens(&ctx.messages);
    let tokens_before = usage_estimate.tokens;
    if !should_compact(
        tokens_before,
        ctx.config.model.context_window,
        &config.settings,
    ) {
        return Ok(());
    }

    let (mut to_summarize, mut keep) = prepare_compaction(&ctx.messages, &config.settings);
    if to_summarize.is_empty() {
        (to_summarize, keep) =
            split_for_compaction_after_usage_anchor(&ctx.messages, usage_estimate.last_usage_index);
    }
    if to_summarize.is_empty() {
        return Ok(());
    }

    let summary = summarize_with_provider_streamer(
        &ctx.config.model,
        &to_summarize,
        config.custom_instructions.as_deref(),
        ctx.config.stream_options.clone(),
        Some(ctx.cancel_token.clone()),
        ctx.config.provider_streamer.clone(),
    )
    .await
    .map_err(|err| AgentTurnError::Compaction(err.to_string()))?;

    let first_kept_message_id = keep.first().map(message_id).unwrap_or("none").to_string();
    for message in &mut keep {
        clear_assistant_usage(message);
    }

    let mut compacted = Vec::with_capacity(1 + keep.len());
    compacted.push(AgentMessage::CompactionSummary {
        message_id: unique_message_id(&ctx.messages, format!("compaction_{}", tokens_before)),
        summary: summary.clone(),
        tokens_before,
    });
    compacted.extend(keep);
    ctx.messages = compacted;

    ctx.runtime_compaction = RuntimeCompactionState {
        summary: Some(summary.clone()),
        first_kept_message_id: Some(first_kept_message_id.clone()),
        tokens_before: Some(tokens_before),
    };
    ctx.emit(AgentEvent::SessionCompacted {
        summary,
        first_kept_message_id,
        tokens_before,
        details: None,
    });

    Ok(())
}

pub(crate) async fn stream_provider(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    let request = ctx
        .provider_request
        .clone()
        .ok_or_else(|| AgentTurnError::Invariant("provider request is not prepared".into()))?;
    let mut llm_stream = stream_model_with_provider_streamer(
        &request.model,
        request.context,
        Some(request.stream_options),
        ctx.config.provider_streamer.clone(),
    );
    let mut assistant_message = None;
    let mut stream_error = None;

    let cancellation_token = ctx.cancel_token.clone();
    while let Some(event) = tokio::select! {
        _ = cancellation_token.clone().cancelled_owned() => return aborted(ctx),
        event = llm_stream.next().fuse() => event,
    } {
        let is_terminal = matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        );
        if let AssistantMessageEvent::Done { message, .. } = &event {
            assistant_message = Some(message.clone());
        }
        if let AssistantMessageEvent::Error { message, .. } = &event {
            stream_error = Some(
                message
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "LLM error".into()),
            );
        }
        ctx.emit(AgentEvent::LlmEvent(event));
        if is_terminal {
            break;
        }
    }

    if let Some(message) = assistant_message {
        ctx.assistant_message = Some(message);
        return Ok(AgentTurnDecision::Next);
    }

    let error = stream_error.unwrap_or_else(|| "LLM stream ended without Done event".into());
    ctx.emit(AgentEvent::AgentError { error });
    Ok(AgentTurnDecision::Error)
}

pub(crate) fn decide_after_assistant(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    let mut assistant = ctx
        .assistant_message
        .clone()
        .ok_or_else(|| AgentTurnError::Invariant("assistant message is not available".into()))?;

    if assistant.stop_reason == StopReason::ToolUse
        && normalize_terminal_tool_arguments(&mut assistant).is_err()
    {
        ctx.emit(AgentEvent::AgentError {
            error: "invalid terminal tool arguments".into(),
        });
        return Ok(AgentTurnDecision::Error);
    }
    if assistant.stop_reason == StopReason::ToolUse
        && let Err(error) = validate_terminal_tool_call_identity(&assistant, &ctx.messages)
    {
        ctx.emit(AgentEvent::AgentError {
            error: error.into(),
        });
        return Ok(AgentTurnDecision::Error);
    }
    ctx.assistant_message = Some(assistant.clone());

    let assistant_id = unique_message_id(
        &ctx.messages,
        assistant
            .response_id
            .clone()
            .unwrap_or_else(|| format!("assistant_{}", ctx.turn)),
    );
    ctx.messages.push(AgentMessage::Assistant {
        message_id: assistant_id,
        message: assistant.clone(),
    });

    match assistant.stop_reason {
        StopReason::Stop | StopReason::Length => Ok(AgentTurnDecision::Continue),
        StopReason::Error => {
            let error = assistant
                .error_message
                .clone()
                .unwrap_or_else(|| "LLM error".into());
            ctx.emit(AgentEvent::AgentError { error });
            Ok(AgentTurnDecision::Error)
        }
        StopReason::Aborted => {
            ctx.emit(AgentEvent::AgentError {
                error: "aborted".into(),
            });
            Ok(AgentTurnDecision::Aborted)
        }
        StopReason::ToolUse => {
            let tool_calls = extract_tool_calls(&assistant);
            if tool_calls.is_empty() {
                ctx.emit(AgentEvent::AgentError {
                    error: "tool-use response contained no tool calls".into(),
                });
                return Ok(AgentTurnDecision::Error);
            }
            ctx.pending_tool_calls = tool_calls
                .into_iter()
                .map(|call| PendingToolCall {
                    index: call.index,
                    id: call.tool_call_id,
                    name: call.tool_name,
                    arguments: call.arguments,
                })
                .collect();
            Ok(AgentTurnDecision::Tools)
        }
    }
}

/// Convert a provider's exact accumulated terminal argument bytes into the
/// executable JSON value. Streaming previews may retain incomplete text, but
/// terminal tool calls must parse without repair, auto-closing, or trailing
/// data before they enter committed history or the execution pipeline.
fn normalize_terminal_tool_arguments(assistant: &mut AssistantMessage) -> Result<(), ()> {
    for block in &mut assistant.content {
        let ContentBlock::ToolCall {
            arguments, kind, ..
        } = block
        else {
            continue;
        };
        if *kind == ai_protocol::api::conversation::ToolCallKind::Custom {
            continue;
        }
        let serde_json::Value::String(raw) = arguments else {
            continue;
        };
        *arguments = parse_terminal_json(raw).map_err(|_| ())?;
    }
    Ok(())
}

fn validate_terminal_tool_call_identity(
    assistant: &AssistantMessage,
    messages: &[AgentMessage],
) -> Result<(), &'static str> {
    let mut used = messages
        .iter()
        .map(AgentMessage::message_id)
        .collect::<HashSet<_>>();
    for message in messages {
        if let AgentMessage::Assistant { message, .. } = message {
            for block in &message.content {
                if let ContentBlock::ToolCall { id, .. } = block {
                    used.insert(id);
                }
            }
        }
    }

    for block in &assistant.content {
        let ContentBlock::ToolCall { id, name, .. } = block else {
            continue;
        };
        if id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err("invalid terminal tool-call identity");
        }
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err("invalid terminal tool-call name");
        }
        if !used.insert(id) {
            return Err("duplicate or reused terminal tool-call identity");
        }
    }
    Ok(())
}

pub(crate) async fn maybe_prepare_next_turn(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    let assistant = ctx
        .assistant_message
        .clone()
        .ok_or_else(|| AgentTurnError::Invariant("assistant message is not available".into()))?;

    match assistant.stop_reason {
        StopReason::Stop | StopReason::Length => {
            let Some(should_stop) = should_stop_after_turn(ctx, &assistant).await? else {
                return Ok(AgentTurnDecision::Error);
            };
            if should_stop {
                ctx.emit(AgentEvent::AgentDone { message: assistant });
                return Ok(AgentTurnDecision::Done);
            }

            if let Some(action) = prepare_next_turn_or_error(ctx).await? {
                return Ok(action);
            }

            let has_more = !ctx.follow_up_queue.is_empty()
                || !ctx.steering_queue.is_empty()
                || !ctx.interjection_queue.is_empty();
            if has_more {
                let follow_ups = drain_queue(&mut ctx.follow_up_queue, ctx.config.follow_up_mode);
                ctx.messages.extend(follow_ups);
                Ok(AgentTurnDecision::Continue)
            } else {
                ctx.emit(AgentEvent::AgentDone { message: assistant });
                Ok(AgentTurnDecision::Done)
            }
        }
        StopReason::ToolUse => {
            let Some(should_stop) = should_stop_after_turn(ctx, &assistant).await? else {
                return Ok(AgentTurnDecision::Error);
            };
            if should_stop {
                ctx.emit(AgentEvent::AgentDone { message: assistant });
                return Ok(AgentTurnDecision::Done);
            }

            if ctx.tool_results_all_terminate {
                ctx.emit(AgentEvent::AgentDone { message: assistant });
                return Ok(AgentTurnDecision::Done);
            }

            if let Some(action) = prepare_next_turn_or_error(ctx).await? {
                return Ok(action);
            }

            Ok(AgentTurnDecision::Continue)
        }
        StopReason::Error => Ok(AgentTurnDecision::Error),
        StopReason::Aborted => Ok(AgentTurnDecision::Aborted),
    }
}

pub(crate) async fn execute_tools(
    ctx: &mut AgentTurnContext,
) -> Result<AgentTurnDecision, AgentTurnError> {
    ctx.tool_results_all_terminate = false;
    let pending = std::mem::take(&mut ctx.pending_tool_calls);
    if pending.is_empty() {
        return Ok(AgentTurnDecision::ContinueProvider);
    }
    if pending.len() > MAX_TOOL_CALLS_PER_TURN {
        return Err(AgentTurnError::ToolLimit(ToolExecutionLimit::CallsPerTurn));
    }

    let requests: Vec<_> = pending
        .iter()
        .map(|call| ToolCallRequest {
            index: call.index,
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect();
    let runtime_tools = ctx
        .tool_runtime
        .as_ref()
        .map(tool_runtime::api::ToolRuntime::definitions)
        .unwrap_or_default();
    let use_sequential =
        should_use_sequential_tools(ctx.config.tool_execution, &requests, &runtime_tools);
    let hook_assistant = if ctx.config.hooks.before_tool_call.is_some()
        || ctx.config.hooks.after_tool_call.is_some()
    {
        ctx.assistant_message.clone().map(Arc::new)
    } else {
        None
    };
    let hook_messages = if hook_assistant.is_some() {
        Some(Arc::<[AgentMessage]>::from(ctx.messages.clone()))
    } else {
        None
    };

    let executions = if use_sequential {
        let mut executions = Vec::with_capacity(pending.len());
        for call in pending {
            let tool = find_executable_tool(ctx.tool_runtime.as_ref(), &call.name);
            let blocked = match invalid_tool_call_result(tool.as_ref(), &call) {
                Some(result) => Some(result),
                None => {
                    before_tool_result(ctx, &call, hook_assistant.clone(), hook_messages.clone())
                        .await
                }
            };
            let result = match blocked {
                Some(result) => result,
                None => {
                    ctx.emit(AgentEvent::ToolCallStart {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                    let result = execute_tool_with_updates(ctx, &call, tool).await;
                    after_tool_result(
                        ctx,
                        &call,
                        result,
                        hook_assistant.clone(),
                        hook_messages.clone(),
                    )
                    .await
                }
            };
            let result = retain_bounded_tool_result(result);

            ctx.emit(AgentEvent::ToolCallEnd {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                result: result.clone(),
            });
            executions.push(ToolCallExecution {
                index: call.index,
                tool_call_id: call.id,
                tool_name: call.name,
                result,
            });
        }
        executions
    } else {
        let after_hook = ctx.config.hooks.after_tool_call.clone();
        let mut prepared = Vec::with_capacity(pending.len());
        for call in pending {
            let tool = find_executable_tool(ctx.tool_runtime.as_ref(), &call.name);
            let blocked = match invalid_tool_call_result(tool.as_ref(), &call) {
                Some(result) => Some(result),
                None => {
                    before_tool_result(ctx, &call, hook_assistant.clone(), hook_messages.clone())
                        .await
                }
            };
            prepared.push((call, tool, blocked));
        }
        for (call, _, blocked) in &prepared {
            if blocked.is_none() {
                ctx.emit(AgentEvent::ToolCallStart {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }
        }

        collect_parallel_tool_executions(ctx, prepared, after_hook, hook_assistant, hook_messages)
            .await
    };

    let all_terminate = !executions.is_empty()
        && executions
            .iter()
            .all(|execution| execution.result.terminate);
    ctx.tool_results
        .extend(executions.iter().map(|execution| execution.result.clone()));
    append_tool_result_messages(&mut ctx.messages, &executions);

    ctx.tool_results_all_terminate = all_terminate;

    Ok(AgentTurnDecision::Continue)
}

async fn collect_parallel_tool_executions(
    ctx: &mut AgentTurnContext,
    prepared: Vec<(
        PendingToolCall,
        Option<ExecutableTool>,
        Option<AgentToolResult>,
    )>,
    after_hook: Option<AfterToolCallHook>,
    assistant_message: Option<Arc<AssistantMessage>>,
    messages: Option<Arc<[AgentMessage]>>,
) -> Vec<ToolCallExecution> {
    let tool_execution_scope = ctx.config.tool_execution_scope.clone();
    let turn = ctx.turn;
    let cancel_token = ctx.cancel_token.clone();
    let mut executions_stream = futures::stream::iter(prepared)
        .map(move |(call, tool, blocked)| {
            let after_hook = after_hook.clone();
            let assistant_message = assistant_message.clone();
            let messages = messages.clone();
            let tool_execution_scope = tool_execution_scope.clone();
            let cancel_token = cancel_token.clone();
            async move {
                let result = match blocked {
                    Some(result) => result,
                    None => {
                        let execution_cancel = cancel_token.child_token();
                        let execution_context = ToolExecutionContext::new(
                            tool_execution_scope,
                            turn,
                            call.id.clone(),
                            call.name.clone(),
                            execution_cancel,
                        );
                        let result =
                            execute_tool(tool, execution_context, call.arguments.clone()).await;
                        apply_after_tool_hook(
                            after_hook,
                            assistant_message,
                            messages,
                            &call,
                            result,
                            cancel_token.clone(),
                        )
                        .await
                    }
                };
                ToolCallExecution {
                    index: call.index,
                    tool_call_id: call.id,
                    tool_name: call.name,
                    result: retain_bounded_tool_result(result),
                }
            }
        })
        .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);

    let mut executions = Vec::new();
    while let Some(execution) = executions_stream.next().await {
        ctx.emit(AgentEvent::ToolCallEnd {
            tool_call_id: execution.tool_call_id.clone(),
            tool_name: execution.tool_name.clone(),
            result: execution.result.clone(),
        });
        executions.push(execution);
    }
    executions.sort_by_key(|execution| execution.index);
    executions
}

async fn before_tool_result(
    ctx: &AgentTurnContext,
    call: &PendingToolCall,
    assistant_message: Option<Arc<AssistantMessage>>,
    messages: Option<Arc<[AgentMessage]>>,
) -> Option<AgentToolResult> {
    let hook = ctx.config.hooks.before_tool_call.clone()?;
    let assistant_message = assistant_message?;
    let messages = messages?;
    let hook_context = BeforeToolCallContext {
        execution_context: ToolExecutionContext::new(
            ctx.config.tool_execution_scope.clone(),
            ctx.turn,
            call.id.clone(),
            call.name.clone(),
            ctx.cancel_token.clone(),
        ),
        assistant_message,
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        arguments: call.arguments.clone(),
        messages,
    };

    let cancellation_token = ctx.cancel_token.clone();
    tokio::select! {
        _ = cancellation_token.clone().cancelled_owned() => {
            Some(error_result(ToolErrorKind::Cancelled, "aborted"))
        },
        result = hook(hook_context) => match result {
            Ok(Some(result)) if result.block => Some(AgentToolResult::error(
                result.reason.unwrap_or_else(|| "blocked".into()),
            )),
            Err(error) => Some(AgentToolResult::error(error)),
            _ => None,
        },
    }
}

async fn after_tool_result(
    ctx: &AgentTurnContext,
    call: &PendingToolCall,
    result: AgentToolResult,
    assistant_message: Option<Arc<AssistantMessage>>,
    messages: Option<Arc<[AgentMessage]>>,
) -> AgentToolResult {
    apply_after_tool_hook(
        ctx.config.hooks.after_tool_call.clone(),
        assistant_message,
        messages,
        call,
        result,
        ctx.cancel_token.clone(),
    )
    .await
}

async fn apply_after_tool_hook(
    hook: Option<AfterToolCallHook>,
    assistant_message: Option<Arc<AssistantMessage>>,
    messages: Option<Arc<[AgentMessage]>>,
    call: &PendingToolCall,
    mut result: AgentToolResult,
    cancellation: CancellationToken,
) -> AgentToolResult {
    let Some(hook) = hook else {
        return result;
    };
    let Some(assistant_message) = assistant_message else {
        return result;
    };
    let Some(messages) = messages else {
        return result;
    };
    let hook_context = AfterToolCallContext {
        assistant_message,
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        arguments: call.arguments.clone(),
        result: result.clone(),
        messages,
    };

    match tokio::select! {
        _ = cancellation.clone().cancelled_owned() => {
            return error_result(ToolErrorKind::Cancelled, "aborted")
        },
        result = hook(hook_context) => result,
    } {
        Ok(Some(after)) => {
            if let Some(content) = after.content {
                result.content = content;
            }
            if let Some(is_error) = after.is_error {
                result.is_error = is_error;
            }
            if let Some(terminate) = after.terminate {
                result.terminate = terminate;
            }
            result
        }
        Err(error) => AgentToolResult::error(error),
        _ => result,
    }
}

fn invalid_tool_call_result(
    tool: Option<&ExecutableTool>,
    call: &PendingToolCall,
) -> Option<AgentToolResult> {
    let Some(tool) = tool else {
        return Some(error_result(
            ToolErrorKind::Unavailable,
            format!("unknown tool: {}", call.name),
        ));
    };
    tool.validate_arguments(&call.arguments)
        .err()
        .map(AgentToolResult::from)
}

fn retain_bounded_tool_result(result: AgentToolResult) -> AgentToolResult {
    if serialized_result_bytes(&result).is_some_and(|bytes| bytes <= MAX_TOOL_RESULT_BYTES_PER_CALL)
    {
        result
    } else {
        error_result(
            ToolErrorKind::Protocol,
            ToolExecutionLimit::Result.message(),
        )
    }
}

fn aborted(ctx: &mut AgentTurnContext) -> Result<AgentTurnDecision, AgentTurnError> {
    ctx.emit(AgentEvent::AgentError {
        error: "aborted".into(),
    });
    Ok(AgentTurnDecision::Aborted)
}

fn unique_message_id(messages: &[AgentMessage], preferred: String) -> String {
    let used = messages
        .iter()
        .map(AgentMessage::message_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    unique_id(&used, preferred)
}

fn unique_id(used: &HashSet<String>, preferred: String) -> String {
    if !used.contains(&preferred) {
        return preferred;
    }
    let mut suffix = 1u64;
    loop {
        let candidate = format!("{preferred}_{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

async fn should_stop_after_turn(
    ctx: &mut AgentTurnContext,
    assistant: &AssistantMessage,
) -> Result<Option<bool>, AgentTurnError> {
    let Some(hook) = ctx.config.hooks.should_stop_after_turn.clone() else {
        return Ok(Some(false));
    };

    match hook(ShouldStopAfterTurnContext {
        messages: ctx.messages.clone(),
        assistant_message: assistant.clone(),
    })
    .await
    {
        Ok(should_stop) => Ok(Some(should_stop)),
        Err(error) => {
            ctx.emit(AgentEvent::AgentError {
                error: error.clone(),
            });
            Ok(None)
        }
    }
}

async fn prepare_next_turn_or_error(
    ctx: &mut AgentTurnContext,
) -> Result<Option<AgentTurnDecision>, AgentTurnError> {
    let Some(hook) = ctx.config.hooks.prepare_next_turn.clone() else {
        return Ok(None);
    };

    let update = match hook(PrepareNextTurnContext {
        messages: ctx.messages.clone(),
        turn: ctx.turn,
    })
    .await
    {
        Ok(update) => update,
        Err(error) => {
            ctx.emit(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Ok(Some(AgentTurnDecision::Error));
        }
    };

    let Some(update) = update else {
        return Ok(None);
    };

    if let Some(messages) = update.messages {
        ctx.messages = messages;
    }
    if let Some(model) = update.model {
        ctx.config.model = model;
    }
    if let Some(thinking_level) = update.thinking_level {
        ctx.config.thinking_level = thinking_level;
    }
    if let Some(stream_options) = update.stream_options {
        ctx.config.stream_options = Some(stream_options);
    }
    Ok(None)
}

fn message_id(message: &AgentMessage) -> &str {
    match message {
        AgentMessage::UserText { message_id, .. }
        | AgentMessage::Assistant { message_id, .. }
        | AgentMessage::ToolResult { message_id, .. }
        | AgentMessage::SystemPrompt { message_id, .. }
        | AgentMessage::CompactionSummary { message_id, .. }
        | AgentMessage::BashExecution { message_id, .. }
        | AgentMessage::Custom { message_id, .. }
        | AgentMessage::BranchSummary { message_id, .. } => message_id,
    }
}

fn clear_assistant_usage(message: &mut AgentMessage) {
    if let AgentMessage::Assistant { message, .. } = message {
        message.usage = Usage::default();
    }
}

fn split_for_compaction_after_usage_anchor(
    messages: &[AgentMessage],
    anchor_index: Option<usize>,
) -> (Vec<AgentMessage>, Vec<AgentMessage>) {
    let Some(anchor_index) = anchor_index else {
        return (vec![], messages.to_vec());
    };
    if messages.is_empty() {
        return (vec![], vec![]);
    }

    let mut split = anchor_index.saturating_add(1).min(messages.len());
    while split < messages.len() && matches!(messages[split], AgentMessage::ToolResult { .. }) {
        split += 1;
    }

    (messages[..split].to_vec(), messages[split..].to_vec())
}
