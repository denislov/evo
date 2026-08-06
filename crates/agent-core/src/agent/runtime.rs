use std::collections::VecDeque;

use ai_protocol::api::conversation::Context;
use ai_protocol::api::stream::StreamOptions;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::{ToolDefinition, ToolDefinitionError};
use tool_runtime::api::ToolRuntime;

use crate::agent::command::{AgentCommand, AgentEventStream, AgentHandle, EVENT_STREAM_CAPACITY};
use crate::agent::queue::enqueue_message;
use crate::agent::turn::TurnRunner;
use crate::agent::turn::context::AgentTurnContext;
use crate::agent::types::{
    AgentConfig, AgentEvent, AgentInputQueue, AgentMessage, AgentQueueError, AgentResources,
    AgentStream,
};
use crate::context::conversion::convert_to_context;
use crate::hooks::BeforeProviderRequestHook;
use crate::resources::{format_prompt_template_invocation, format_skill_invocation};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentAdmissionError {
    #[error("agent is busy while starting {operation}")]
    Busy { operation: &'static str },
    #[error("cannot continue: no messages in context")]
    EmptyContext,
    #[error("cannot continue from message role: assistant")]
    AssistantTail,
    #[error("invalid agent configuration: {message}")]
    InvalidConfig { message: String },
}

pub struct AgentState {
    pub messages: Vec<AgentMessage>,
    pub tool_runtime: Option<ToolRuntime>,
    pub provider_tools: Vec<ToolDefinition>,
    pub config: AgentConfig,
    pub cancel_token: CancellationToken,
    pub steering_queue: VecDeque<AgentMessage>,
    pub follow_up_queue: VecDeque<AgentMessage>,
    pub(crate) provider_request_override: Option<ProviderRequestOverride>,
}

pub(crate) struct ProviderRequestOverride {
    pub context: Context,
    pub stream_options: Option<StreamOptions>,
}

/// A handle to the agent actor task.
///
/// The actor owns `AgentState` exclusively; every public method forwards a
/// command over a bounded mailbox. Synchronous methods fire-and-forget and
/// surface only mailbox saturation or actor shutdown; methods that need a
/// result (admission, queries, validation) are `async` and await the actor's
/// reply.
#[derive(Clone)]
pub struct Agent {
    handle: AgentHandle,
}

impl Agent {
    pub fn new(config: AgentConfig) -> Self {
        let (sender, receiver) = crate::agent::command::mailbox();
        let state = AgentState {
            messages: Vec::new(),
            tool_runtime: None,
            provider_tools: Vec::new(),
            cancel_token: CancellationToken::new(),
            config,
            steering_queue: VecDeque::new(),
            follow_up_queue: VecDeque::new(),
            provider_request_override: None,
        };
        tokio::spawn(run_actor(state, receiver));
        Self {
            handle: AgentHandle { commands: sender },
        }
    }

    pub async fn add_provider_tool(
        &self,
        definition: ToolDefinition,
    ) -> Result<(), ToolDefinitionError> {
        self.handle
            .add_provider_tool(definition)
            .await
            .unwrap_or_else(|_| Err(ToolDefinitionError::new("actor", "agent actor is closed")))
    }

    pub async fn set_tool_runtime(&self, runtime: ToolRuntime) -> Result<(), ToolDefinitionError> {
        self.handle
            .set_tool_runtime(runtime)
            .await
            .unwrap_or_else(|_| Err(ToolDefinitionError::new("actor", "agent actor is closed")))
    }

    pub fn add_message(&self, msg: AgentMessage) {
        self.handle.add_message(msg);
    }

    pub async fn messages(&self) -> Vec<AgentMessage> {
        self.handle.messages().await.unwrap_or_default()
    }

    pub async fn before_provider_request_hook(&self) -> Option<BeforeProviderRequestHook> {
        self.handle
            .before_provider_request_hook()
            .await
            .unwrap_or(None)
    }

    pub fn set_before_provider_request_hook(&self, hook: Option<BeforeProviderRequestHook>) {
        self.handle.set_before_provider_request_hook(hook);
    }

    pub fn set_resources(&self, resources: AgentResources) {
        self.handle.set_resources(resources);
    }

    pub fn steer(&self, text: impl Into<String>) -> Result<(), AgentQueueError> {
        self.handle.steer(text.into())
    }

    pub fn steer_content(
        &self,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        self.handle.steer_content(content)
    }

    pub fn follow_up(&self, text: impl Into<String>) -> Result<(), AgentQueueError> {
        self.handle.follow_up(text.into())
    }

    pub fn follow_up_content(
        &self,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        self.handle.follow_up_content(content)
    }

    pub fn clear_queues(&self) {
        self.handle.clear_queues();
    }

    /// Drain and return all queued steering messages.
    pub async fn drain_steering_queue(&self) -> Vec<AgentMessage> {
        self.handle.drain_steering_queue().await.unwrap_or_default()
    }

    /// Drain and return all queued follow-up messages.
    pub async fn drain_follow_up_queue(&self) -> Vec<AgentMessage> {
        self.handle
            .drain_follow_up_queue()
            .await
            .unwrap_or_default()
    }

    pub async fn skill(
        &self,
        name: &str,
        additional_instructions: Option<&str>,
    ) -> Result<AgentStream, String> {
        let resources = self
            .handle
            .resources()
            .await
            .map_err(|_| "agent actor is closed".to_string())?;
        let skill = resources
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("skill '{name}' not found"))?;
        let prompt = format_skill_invocation(
            &skill.name,
            &skill.location,
            &skill.content,
            additional_instructions,
        );
        self.handle
            .try_prompt(prompt)
            .await
            .map(stream_from_receiver)
            .map_err(|error| error.to_string())
    }

    pub async fn prompt_from_template(
        &self,
        name: &str,
        args: &[String],
    ) -> Result<AgentStream, String> {
        let resources = self
            .handle
            .resources()
            .await
            .map_err(|_| "agent actor is closed".to_string())?;
        let template = resources
            .prompt_templates
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| format!("prompt template '{name}' not found"))?;
        let prompt = format_prompt_template_invocation(&template.name, &template.content, args);
        self.handle
            .try_prompt(prompt)
            .await
            .map(stream_from_receiver)
            .map_err(|error| error.to_string())
    }

    /// Adds a UserText message and runs the full tool-calling loop.
    /// Returns an AgentStream that yields events until the model stops
    /// or an error occurs.
    ///
    /// The stream is lazy: the admission command is sent on first poll, so a
    /// busy or closed agent surfaces as an `AgentError` event instead of a
    /// synchronous error.
    pub fn prompt(&self, text: &str) -> AgentStream {
        let handle = self.handle.clone();
        let text = text.to_string();
        Box::pin(async_stream::stream! {
            let mut stream = match handle.try_prompt(text).await {
                Ok(stream) => stream,
                Err(error) => {
                    yield AgentEvent::AgentError {
                        error: error.to_string(),
                    };
                    return;
                }
            };
            while let Some(event) = stream.recv().await {
                yield event;
            }
        })
    }

    pub async fn try_prompt(&self, text: &str) -> Result<AgentEventStream, AgentAdmissionError> {
        self.handle.try_prompt(text.to_string()).await
    }

    /// Runs the model/tool loop with the messages already present on the agent.
    /// Harness code uses this when it needs to transform or patch messages before
    /// starting a turn.
    ///
    /// Mirrors TS `agentLoopContinue`: returns an error stream if `messages` is
    /// empty or the last message is an assistant message. Like [`Agent::prompt`],
    /// the stream is lazy and reports admission failures as `AgentError` events.
    pub fn run(&self) -> Result<AgentStream, String> {
        let handle = self.handle.clone();
        Ok(Box::pin(async_stream::stream! {
            let mut stream = match handle.try_run().await {
                Ok(stream) => stream,
                Err(error) => {
                    yield AgentEvent::AgentError {
                        error: error.to_string(),
                    };
                    return;
                }
            };
            while let Some(event) = stream.recv().await {
                yield event;
            }
        }))
    }

    pub async fn try_run(&self) -> Result<AgentEventStream, AgentAdmissionError> {
        self.handle.try_run().await
    }

    pub fn with_messages(config: AgentConfig, messages: Vec<AgentMessage>) -> Self {
        let agent = Self::new(config);
        agent.replace_messages(messages);
        agent
    }

    pub fn replace_messages(&self, messages: Vec<AgentMessage>) {
        self.handle.replace_messages(messages);
    }

    pub async fn provider_request_snapshot(&self) -> (Context, Option<StreamOptions>) {
        self.handle
            .provider_request_snapshot()
            .await
            .unwrap_or_else(|_| {
                (
                    Context {
                        system_prompt: None,
                        messages: Vec::new(),
                        tools: None,
                    },
                    None,
                )
            })
    }

    pub fn set_provider_request_override(
        &self,
        context: Context,
        stream_options: Option<StreamOptions>,
    ) {
        self.handle
            .set_provider_request_override(context, stream_options);
    }

    /// Cancels an in-flight loop. Safe to call from another task.
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Gracefully stops the actor: any in-flight turn is committed and the
    /// actor task ends. All later calls fail with the closed-actor errors
    /// (`AgentQueueError::ActorClosed`, `AgentAdmissionError::Busy`).
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }
}

fn stream_from_receiver(receiver: AgentEventStream) -> AgentStream {
    // `tokio::sync::mpsc::Receiver` is not a `Stream`; bridge it with an
    // async unfold so the typed `AgentStream` alias keeps working.
    Box::pin(futures::stream::unfold(
        receiver,
        |mut receiver| async move { receiver.recv().await.map(|event| (event, receiver)) },
    ))
}

pub(crate) fn next_message_id(
    messages: &[AgentMessage],
    steering_queue: &VecDeque<AgentMessage>,
    follow_up_queue: &VecDeque<AgentMessage>,
    prefix: &str,
) -> String {
    let mut index = 0u64;
    loop {
        let candidate = format!("{prefix}_{index}");
        if !messages
            .iter()
            .chain(steering_queue.iter())
            .chain(follow_up_queue.iter())
            .any(|message| message.message_id() == candidate)
        {
            return candidate;
        }
        index += 1;
    }
}

fn next_message_id_from_preferred(state: &AgentState, preferred: String) -> String {
    if !state
        .messages
        .iter()
        .chain(state.steering_queue.iter())
        .chain(state.follow_up_queue.iter())
        .any(|message| message.message_id() == preferred)
    {
        return preferred;
    }
    let mut index = 1u64;
    loop {
        let candidate = format!("{preferred}_{index}");
        let used = state
            .messages
            .iter()
            .chain(state.steering_queue.iter())
            .chain(state.follow_up_queue.iter())
            .any(|message| message.message_id() == candidate);
        if !used {
            return candidate;
        }
        index += 1;
    }
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
    loop {
        // The consumer dropped its event stream mid-turn: commit promptly so
        // the messages are visible and a new turn can be admitted. This also
        // closes the race between `drop(stream)` and a follow-up query.
        if let (Some(tx), true) = (event_tx.as_ref(), turn.is_some())
            && tx.is_closed()
        {
            commit_turn(&mut state, &mut turn);
            event_tx = None;
        }
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(command) => {
                        // Re-check consumer drop before processing commands
                        // (e.g., Messages) so the turn's messages are committed.
                        if let (Some(tx), true) = (event_tx.as_ref(), turn.is_some())
                            && tx.is_closed()
                        {
                            commit_turn(&mut state, &mut turn);
                            event_tx = None;
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
                        let tx = event_tx.as_mut().expect("event sender is set");
                        if tx.send(event).await.is_err() {
                            commit_turn(&mut state, &mut turn);
                            event_tx = None;
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
                                    while let Ok(command) = commands.try_recv() {
                                        if handle_command(&mut state, &mut turn, &mut event_tx, command).await {
                                            return;
                                        }
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
    // Graceful shutdown: commit any in-flight turn so nothing is lost.
    commit_turn(&mut state, &mut turn);
}

/// Commits the in-flight turn's working copy back into the actor state.
fn commit_turn(state: &mut AgentState, turn: &mut Option<TurnRunner>) {
    if let Some(runner) = turn.take() {
        let mut context = runner.into_context();
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
                None => {
                    let message_id = next_message_id(
                        &state.messages,
                        &state.steering_queue,
                        &state.follow_up_queue,
                        "steer",
                    );
                    enqueue_message(
                        &mut state.steering_queue,
                        AgentInputQueue::Steering,
                        AgentMessage::UserText { message_id, text },
                    )
                }
            };
            let _ = reply.send(result);
        }
        AgentCommand::SteerContent { content, reply } => {
            let result = match turn {
                Some(runner) => runner.steer_content(content),
                None => {
                    let message_id = next_message_id(
                        &state.messages,
                        &state.steering_queue,
                        &state.follow_up_queue,
                        "steer",
                    );
                    enqueue_message(
                        &mut state.steering_queue,
                        AgentInputQueue::Steering,
                        AgentMessage::Custom {
                            message_id,
                            custom_type: "input".into(),
                            content,
                            display: true,
                            details: None,
                            timestamp: 0,
                        },
                    )
                }
            };
            let _ = reply.send(result);
        }
        AgentCommand::FollowUp { text, reply } => {
            let result = match turn {
                Some(runner) => runner.follow_up(text),
                None => {
                    let message_id = next_message_id(
                        &state.messages,
                        &state.steering_queue,
                        &state.follow_up_queue,
                        "followup",
                    );
                    enqueue_message(
                        &mut state.follow_up_queue,
                        AgentInputQueue::FollowUp,
                        AgentMessage::UserText { message_id, text },
                    )
                }
            };
            let _ = reply.send(result);
        }
        AgentCommand::FollowUpContent { content, reply } => {
            let result = match turn {
                Some(runner) => runner.follow_up_content(content),
                None => {
                    let message_id = next_message_id(
                        &state.messages,
                        &state.steering_queue,
                        &state.follow_up_queue,
                        "followup",
                    );
                    enqueue_message(
                        &mut state.follow_up_queue,
                        AgentInputQueue::FollowUp,
                        AgentMessage::Custom {
                            message_id,
                            custom_type: "input".into(),
                            content,
                            display: true,
                            details: None,
                            timestamp: 0,
                        },
                    )
                }
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
            if let Some(runner) = turn {
                runner.clear_queues();
            }
            let _ = reply.send(());
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
            let mut drained = state.steering_queue.drain(..).collect::<Vec<_>>();
            if let Some(runner) = turn {
                drained.extend(runner.drain_steering_queue());
            }
            let _ = reply.send(drained);
        }
        AgentCommand::DrainFollowUpQueue { reply } => {
            let mut drained = state.follow_up_queue.drain(..).collect::<Vec<_>>();
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
