use ai_protocol::api::conversation::{AssistantMessage, StopReason};

use crate::agent::queue::drain_queue;
use crate::agent::types::{AgentEvent, AgentMessage};
use crate::hooks::{PrepareNextTurnContext, ShouldStopAfterTurnContext};

use super::super::context::AgentTurnContext;
use super::{AgentTurnDecision, AgentTurnError};

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
        Ok(outcome) => {
            if !outcome.should_stop && !outcome.additional_context.is_empty() {
                let turn = ctx.turn;
                ctx.messages
                    .extend(outcome.additional_context.into_iter().enumerate().map(
                        |(index, text)| AgentMessage::UserText {
                            message_id: format!("hook_additional_context_{turn}_{index}"),
                            text,
                        },
                    ));
            }
            Ok(Some(outcome.should_stop))
        }
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
