use ai_protocol::api::conversation::Context;
use ai_protocol::api::stream::StreamOptions;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::{ToolDefinition, ToolDefinitionError};
use tool_runtime::api::ToolRuntime;

use crate::agent::command::{AgentCommand, AgentEventStream, EVENT_STREAM_CAPACITY};
use crate::agent::queue::{PromptQueueEntry, edit_entry, enqueue_message, remove_entry};
use crate::agent::runtime::{
    AgentAdmissionError, AgentState, ProviderRequestOverride, next_message_id,
    next_message_id_from_preferred,
};
use crate::agent::turn::TurnRunner;
use crate::agent::turn::context::AgentTurnContext;
use crate::agent::types::{AgentEvent, AgentInputQueue, AgentMessage, AgentQueueError};
use crate::context::conversion::convert_to_context;

fn enqueue_text_entry(
    state: &mut AgentState,
    queue_kind: AgentInputQueue,
    prefix: &str,
    text: String,
) -> Result<(), AgentQueueError> {
    let message_id = next_message_id(
        &state.messages,
        &state.steering_queue,
        &state.follow_up_queue,
        &state.interjection_queue,
        prefix,
    );
    let queue = match queue_kind {
        AgentInputQueue::Steering => &mut state.steering_queue,
        AgentInputQueue::FollowUp => &mut state.follow_up_queue,
        AgentInputQueue::Interjection => &mut state.interjection_queue,
    };
    enqueue_message(
        queue,
        queue_kind,
        PromptQueueEntry {
            id: message_id.clone(),
            version: 0,
            message: AgentMessage::UserText { message_id, text },
        },
    )
}

fn enqueue_content_entry(
    state: &mut AgentState,
    queue_kind: AgentInputQueue,
    prefix: &str,
    content: Vec<ai_protocol::api::conversation::ContentBlock>,
) -> Result<(), AgentQueueError> {
    let message_id = next_message_id(
        &state.messages,
        &state.steering_queue,
        &state.follow_up_queue,
        &state.interjection_queue,
        prefix,
    );
    let queue = match queue_kind {
        AgentInputQueue::Steering => &mut state.steering_queue,
        AgentInputQueue::FollowUp => &mut state.follow_up_queue,
        AgentInputQueue::Interjection => &mut state.interjection_queue,
    };
    enqueue_message(
        queue,
        queue_kind,
        PromptQueueEntry {
            id: message_id.clone(),
            version: 0,
            message: AgentMessage::Custom {
                message_id,
                custom_type: "input".into(),
                content,
                display: true,
                details: None,
                timestamp: 0,
            },
        },
    )
}

// ── Actor task ────────────────────────────────────────────────

/// The actor loop. Owns `AgentState` exclusively and interleaves command
/// handling with turn advancement via `tokio::select!`.
///
/// While a turn is running it lives in a [`TurnRunner`] holding a working
/// copy; steer/follow-up commands arriving mid-turn are appended directly to
/// that working copy (no locks), and the turn's queue-drain nodes consume
/// them. When the loop finishes (or the consumer drops the event stream, or
/// the actor shuts down), the working copy is committed back into the state.
pub(crate) async fn run_actor(mut state: AgentState, mut commands: mpsc::Receiver<AgentCommand>) {
    let mut turn: Option<TurnRunner> = None;
    let mut event_tx: Option<mpsc::Sender<AgentEvent>> = None;
    let mut pending_commit = false;
    loop {
        // The consumer dropped its event stream mid-turn: abort the turn so
        // it completes promptly, then drain remaining events and commit once
        // the context is restored. This also closes the race between
        // `drop(stream)` and a follow-up query.
        if !pending_commit && event_tx.as_ref().is_some_and(|tx| tx.is_closed()) && turn.is_some() {
            if let Some(runner) = turn.as_mut() {
                runner.abort();
            }
            pending_commit = true;
            event_tx = None;
        }
        // Pending commit: advance the turn to completion, then commit. The
        // consumer is gone, so events are discarded.
        if pending_commit {
            if let Some(runner) = turn.as_mut()
                && runner.next_event().await.is_some()
            {
                continue;
            }
            commit_turn(&mut state, &mut turn);
            pending_commit = false;
            continue;
        }
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(command) => {
                        // Re-check consumer drop before processing commands
                        // (e.g., Messages) so the turn's messages are committed.
                        if event_tx.as_ref().is_some_and(|tx| tx.is_closed()) && turn.is_some() {
                            if let Some(runner) = turn.as_mut() {
                                runner.abort();
                            }
                            pending_commit = true;
                            event_tx = None;
                            continue;
                        }
                        if handle_command(&mut state, &mut turn, &mut event_tx, command).await {
                            break;
                        }
                    }
                    None => break,
                }
            }
            event = async { turn.as_mut()?.next_event().await }, if turn.is_some() => {
                match event {
                    Some(event) => {
                        // Send the event to the consumer without letting a
                        // full event channel starve the mailbox: while the
                        // send is pending, keep handling commands so a reply
                        // awaited by the consumer (steer/follow-up) cannot
                        // deadlock against the consumer waiting for that
                        // reply while the actor waits for channel space.
                        let tx = event_tx.take().expect("event sender is set");
                        let send = Box::pin(tx.send(event));
                        let mut send_pending = send;
                        let mut shutting_down = false;
                        loop {
                            tokio::select! {
                                _ = &mut send_pending => break,
                                command = commands.recv() => {
                                    match command {
                                        Some(command) => {
                                            if handle_command(&mut state, &mut turn, &mut event_tx, command).await {
                                                shutting_down = true;
                                                break;
                                            }
                                        }
                                        None => break,
                                    }
                                }
                            }
                        }
                        drop(send_pending);
                        if tx.is_closed() && turn.is_some() {
                            // Consumer dropped the stream mid-send: abort the
                            // turn and commit once the context is restored.
                            if let Some(runner) = turn.as_mut() {
                                runner.abort();
                            }
                            pending_commit = true;
                        } else if !shutting_down {
                            event_tx = Some(tx);
                        }
                        if shutting_down {
                            break;
                        }
                    }
                    None => {
                        if let Some(runner) = turn.as_mut() {
                            if runner.turn_continues() {
                                // Yield via the timer wheel so the consumer
                                // gets polled before the next turn starts.
                                // This lets it process events, drop the
                                // stream, or enqueue steering input.
                                tokio::time::sleep(std::time::Duration::from_micros(1)).await;
                                if event_tx.as_ref().is_some_and(|tx| tx.is_closed()) {
                                    commit_turn(&mut state, &mut turn);
                                    event_tx = None;
                                } else {
                                    // Drain any commands that arrived during the yield.
                                    // A Shutdown command must not return early here:
                                    // the graceful shutdown block below aborts the
                                    // in-flight turn and commits its working copy.
                                    let mut shutting_down = false;
                                    while let Ok(command) = commands.try_recv() {
                                        if handle_command(&mut state, &mut turn, &mut event_tx, command).await {
                                            shutting_down = true;
                                            break;
                                        }
                                    }
                                    if shutting_down {
                                        break;
                                    }
                                    if event_tx.as_ref().is_some_and(|tx| tx.is_closed()) {
                                        commit_turn(&mut state, &mut turn);
                                        event_tx = None;
                                    } else if let Some(runner) = turn.as_mut() {
                                        runner.start_next_turn();
                                    }
                                }
                            } else {
                                commit_turn(&mut state, &mut turn);
                                event_tx = None;
                            }
                        } else {
                            commit_turn(&mut state, &mut turn);
                            event_tx = None;
                        }
                    }
                }
            }
        }
    }
    // Graceful shutdown: abort any in-flight turn, drain events, and commit.
    if let Some(runner) = turn.as_mut() {
        runner.abort();
        while runner.next_event().await.is_some() {}
    }
    commit_turn(&mut state, &mut turn);
}

/// Commits the in-flight turn's working copy back into the actor state.
fn commit_turn(state: &mut AgentState, turn: &mut Option<TurnRunner>) {
    if let Some(runner) = turn.take() {
        let (mut context, pending) = runner.into_context();
        // Pending entries arrived while the turn future was running, after
        // the working copy's last queue drain, so they follow the residual
        // working-copy entries in FIFO order. They are appended without a
        // capacity re-check: each pending queue was already bounded at
        // enqueue time, and dropping input on commit would silently lose a
        // user's steer/follow-up.
        context.steering_queue.extend(pending.steering);
        context.follow_up_queue.extend(pending.follow_up);
        context.interjection_queue.extend(pending.interjection);
        context.apply_to_state(state);
    }
}

/// Starts a turn: snapshots the state into a working copy, moves the live
/// queues into it, and hands the consumer a fresh bounded event stream.
fn start_turn(
    state: &mut AgentState,
    turn: &mut Option<TurnRunner>,
    event_tx: &mut Option<mpsc::Sender<AgentEvent>>,
) -> AgentEventStream {
    let context = AgentTurnContext::from_state(state);
    state.steering_queue.clear();
    state.follow_up_queue.clear();
    state.interjection_queue.clear();
    let (tx, rx) = mpsc::channel(EVENT_STREAM_CAPACITY);
    *turn = Some(TurnRunner::new(context));
    *event_tx = Some(tx);
    rx
}

fn admit_prompt(
    state: &mut AgentState,
    turn: &mut Option<TurnRunner>,
    event_tx: &mut Option<mpsc::Sender<AgentEvent>>,
    text: String,
) -> Result<AgentEventStream, AgentAdmissionError> {
    state
        .config
        .validate()
        .map_err(|error| AgentAdmissionError::InvalidConfig {
            message: error.to_string(),
        })?;
    if turn.is_some() {
        return Err(AgentAdmissionError::Busy {
            operation: "prompt",
        });
    }
    state.cancel_token = CancellationToken::new();
    let message_id = next_message_id(
        &state.messages,
        &state.steering_queue,
        &state.follow_up_queue,
        &state.interjection_queue,
        "user",
    );
    state
        .messages
        .push(AgentMessage::UserText { message_id, text });
    Ok(start_turn(state, turn, event_tx))
}

fn admit_run(
    state: &mut AgentState,
    turn: &mut Option<TurnRunner>,
    event_tx: &mut Option<mpsc::Sender<AgentEvent>>,
) -> Result<AgentEventStream, AgentAdmissionError> {
    state
        .config
        .validate()
        .map_err(|error| AgentAdmissionError::InvalidConfig {
            message: error.to_string(),
        })?;
    if state.messages.is_empty() {
        return Err(AgentAdmissionError::EmptyContext);
    }
    if matches!(state.messages.last(), Some(AgentMessage::Assistant { .. })) {
        return Err(AgentAdmissionError::AssistantTail);
    }
    if turn.is_some() {
        return Err(AgentAdmissionError::Busy { operation: "run" });
    }
    state.cancel_token = CancellationToken::new();
    Ok(start_turn(state, turn, event_tx))
}

/// Returns `true` when the actor should shut down (Shutdown command).
async fn handle_command(
    state: &mut AgentState,
    turn: &mut Option<TurnRunner>,
    event_tx: &mut Option<mpsc::Sender<AgentEvent>>,
    command: AgentCommand,
) -> bool {
    match command {
        AgentCommand::Prompt { text, reply } => {
            let result = admit_prompt(state, turn, event_tx, text);
            let _ = reply.send(result);
        }
        AgentCommand::Run { reply } => {
            let result = admit_run(state, turn, event_tx);
            let _ = reply.send(result);
        }
        AgentCommand::Steer { text, reply } => {
            let result = match turn {
                Some(runner) => runner.steer(text),
                None => enqueue_text_entry(state, AgentInputQueue::Steering, "steer", text),
            };
            let _ = reply.send(result);
        }
        AgentCommand::SteerContent { content, reply } => {
            let result = match turn {
                Some(runner) => runner.steer_content(content),
                None => enqueue_content_entry(state, AgentInputQueue::Steering, "steer", content),
            };
            let _ = reply.send(result);
        }
        AgentCommand::FollowUp { text, reply } => {
            let result = match turn {
                Some(runner) => runner.follow_up(text),
                None => enqueue_text_entry(state, AgentInputQueue::FollowUp, "followup", text),
            };
            let _ = reply.send(result);
        }
        AgentCommand::FollowUpContent { content, reply } => {
            let result = match turn {
                Some(runner) => runner.follow_up_content(content),
                None => {
                    enqueue_content_entry(state, AgentInputQueue::FollowUp, "followup", content)
                }
            };
            let _ = reply.send(result);
        }
        AgentCommand::Interject { text, reply } => {
            let result = match turn {
                Some(runner) => runner.interject(text),
                None => enqueue_text_entry(state, AgentInputQueue::Interjection, "interject", text),
            };
            let _ = reply.send(result);
        }
        AgentCommand::InterjectContent { content, reply } => {
            let result = match turn {
                Some(runner) => runner.interject_content(content),
                None => enqueue_content_entry(
                    state,
                    AgentInputQueue::Interjection,
                    "interject",
                    content,
                ),
            };
            let _ = reply.send(result);
        }
        AgentCommand::Abort { reply } => {
            match turn {
                Some(runner) => runner.abort(),
                None => state.cancel_token.cancel(),
            }
            let _ = reply.send(());
        }
        AgentCommand::ClearQueues { reply } => {
            state.steering_queue.clear();
            state.follow_up_queue.clear();
            state.interjection_queue.clear();
            if let Some(runner) = turn {
                runner.clear_queues();
            }
            let _ = reply.send(());
        }
        AgentCommand::EditQueueEntry {
            entry_id,
            expected_version,
            new_message,
            reply,
        } => {
            let result = match turn {
                Some(runner) => runner.edit_queue_entry(&entry_id, expected_version, new_message),
                None => edit_entry(
                    &mut [
                        &mut state.steering_queue,
                        &mut state.follow_up_queue,
                        &mut state.interjection_queue,
                    ],
                    &entry_id,
                    expected_version,
                    new_message,
                ),
            };
            let _ = reply.send(result);
        }
        AgentCommand::RemoveQueueEntry {
            entry_id,
            expected_version,
            reply,
        } => {
            let result = match turn {
                Some(runner) => runner.remove_queue_entry(&entry_id, expected_version),
                None => remove_entry(
                    &mut [
                        &mut state.steering_queue,
                        &mut state.follow_up_queue,
                        &mut state.interjection_queue,
                    ],
                    &entry_id,
                    expected_version,
                ),
            };
            let _ = reply.send(result);
        }
        AgentCommand::Messages { reply } => {
            let _ = reply.send(state.messages.clone());
        }
        AgentCommand::AddMessage { message, reply } => {
            let mut message = message;
            let preferred = message.message_id().to_owned();
            message.set_message_id(next_message_id_from_preferred(state, preferred));
            state.messages.push(message);
            let _ = reply.send(());
        }
        AgentCommand::ReplaceMessages { messages, reply } => {
            state.messages = messages;
            let _ = reply.send(());
        }
        AgentCommand::SetToolRuntime { runtime, reply } => {
            let result = set_tool_runtime_on_state(state, runtime);
            let _ = reply.send(result);
        }
        AgentCommand::AddProviderTool { definition, reply } => {
            let result = add_provider_tool_to_state(state, definition);
            let _ = reply.send(result);
        }
        AgentCommand::SetResources { resources, reply } => {
            state.config.resources = resources;
            let _ = reply.send(());
        }
        AgentCommand::ProviderRequestSnapshot { reply } => {
            let snapshot = provider_request_snapshot_from_state(state);
            let _ = reply.send(snapshot);
        }
        AgentCommand::SetProviderRequestOverride {
            context,
            stream_options,
            reply,
        } => {
            state.provider_request_override = Some(ProviderRequestOverride {
                context: (*context).clone(),
                stream_options: stream_options.clone(),
            });
            if let Some(runner) = turn {
                runner.set_provider_request_override(*context, stream_options);
            }
            let _ = reply.send(());
        }
        AgentCommand::BeforeProviderRequestHook { reply } => {
            let _ = reply.send(state.config.hooks.before_provider_request.clone());
        }
        AgentCommand::SetBeforeProviderRequestHook { hook, reply } => {
            state.config.hooks.before_provider_request = hook;
            let _ = reply.send(());
        }
        AgentCommand::DrainSteeringQueue { reply } => {
            let mut drained: Vec<AgentMessage> = state
                .steering_queue
                .drain(..)
                .map(|entry| entry.message)
                .collect();
            if let Some(runner) = turn {
                drained.extend(runner.drain_steering_queue());
            }
            let _ = reply.send(drained);
        }
        AgentCommand::DrainFollowUpQueue { reply } => {
            let mut drained: Vec<AgentMessage> = state
                .follow_up_queue
                .drain(..)
                .map(|entry| entry.message)
                .collect();
            if let Some(runner) = turn {
                drained.extend(runner.drain_follow_up_queue());
            }
            let _ = reply.send(drained);
        }
        AgentCommand::Resources { reply } => {
            let _ = reply.send(state.config.resources.clone());
        }
        AgentCommand::Shutdown => return true,
    }
    false
}

fn set_tool_runtime_on_state(
    state: &mut AgentState,
    runtime: ToolRuntime,
) -> Result<(), ToolDefinitionError> {
    let definitions = runtime.definitions();
    for definition in &definitions {
        if definition.capabilities.provider_executed {
            return Err(ToolDefinitionError::new(
                "capabilities",
                format!(
                    "provider-executed tool {} cannot enter the local runtime",
                    definition.id
                ),
            ));
        }
        if state
            .provider_tools
            .iter()
            .any(|tool| tool.id == definition.id)
        {
            return Err(ToolDefinitionError::new(
                "id",
                format!("duplicate tool id: {}", definition.id),
            ));
        }
    }
    state.tool_runtime = Some(runtime);
    Ok(())
}

fn add_provider_tool_to_state(
    state: &mut AgentState,
    definition: ToolDefinition,
) -> Result<(), ToolDefinitionError> {
    definition.validate()?;
    if !definition.capabilities.provider_executed {
        return Err(ToolDefinitionError::new(
            "capabilities",
            "provider declaration must set provider_executed",
        ));
    }
    if state
        .provider_tools
        .iter()
        .any(|existing| existing.id == definition.id)
        || state
            .tool_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.definition(&definition.id).is_some())
    {
        return Err(ToolDefinitionError::new(
            "id",
            format!("duplicate tool id: {}", definition.id),
        ));
    }
    state.provider_tools.push(definition);
    Ok(())
}

fn provider_request_snapshot_from_state(state: &AgentState) -> (Context, Option<StreamOptions>) {
    let runtime_tools = state
        .tool_runtime
        .as_ref()
        .map(ToolRuntime::definitions)
        .unwrap_or_default();
    let context = convert_to_context(
        &state.config.system_prompt,
        &state.messages,
        &runtime_tools,
        &state.provider_tools,
        &state.config.resources,
    );
    (context, state.config.stream_options.clone())
}
