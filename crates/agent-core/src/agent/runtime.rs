use std::collections::VecDeque;

use ai_protocol::api::conversation::Context;
use ai_protocol::api::stream::StreamOptions;
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::{ToolDefinition, ToolDefinitionError};
use tool_runtime::api::ToolRuntime;

use crate::agent::actor::run_actor;
use crate::agent::command::{AgentEventStream, AgentHandle};
use crate::agent::queue::PromptQueueEntry;
use crate::agent::types::{AgentConfig, AgentEvent, AgentMessage, AgentResources, AgentStream};
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
    pub steering_queue: VecDeque<PromptQueueEntry>,
    pub follow_up_queue: VecDeque<PromptQueueEntry>,
    pub interjection_queue: VecDeque<PromptQueueEntry>,
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
    pub(crate) handle: AgentHandle,
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
            interjection_queue: VecDeque::new(),
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
    steering_queue: &VecDeque<PromptQueueEntry>,
    follow_up_queue: &VecDeque<PromptQueueEntry>,
    interjection_queue: &VecDeque<PromptQueueEntry>,
    prefix: &str,
) -> String {
    let mut index = 0u64;
    loop {
        let candidate = format!("{prefix}_{index}");
        if !messages
            .iter()
            .chain(steering_queue.iter().map(|entry| &entry.message))
            .chain(follow_up_queue.iter().map(|entry| &entry.message))
            .chain(interjection_queue.iter().map(|entry| &entry.message))
            .any(|message| message.message_id() == candidate)
        {
            return candidate;
        }
        index += 1;
    }
}

pub(crate) fn next_message_id_from_preferred(state: &AgentState, preferred: String) -> String {
    if !state
        .messages
        .iter()
        .chain(state.steering_queue.iter().map(|entry| &entry.message))
        .chain(state.follow_up_queue.iter().map(|entry| &entry.message))
        .chain(state.interjection_queue.iter().map(|entry| &entry.message))
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
            .chain(state.steering_queue.iter().map(|entry| &entry.message))
            .chain(state.follow_up_queue.iter().map(|entry| &entry.message))
            .chain(state.interjection_queue.iter().map(|entry| &entry.message))
            .any(|message| message.message_id() == candidate);
        if !used {
            return candidate;
        }
        index += 1;
    }
}
