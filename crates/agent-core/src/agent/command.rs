use ai_protocol::api::conversation::{ContentBlock, Context};
use ai_protocol::api::stream::StreamOptions;
use tokio::sync::{mpsc, oneshot};
use tool_contract::api::definition::{ToolDefinition, ToolDefinitionError};
use tool_runtime::api::ToolRuntime;

use crate::agent::AgentAdmissionError;
use crate::agent::types::{AgentEvent, AgentMessage, AgentQueueError, AgentResources};
use crate::hooks::BeforeProviderRequestHook;

/// Bounded event stream consumed by callers of `AgentHandle::prompt` / `run`.
pub type AgentEventStream = mpsc::Receiver<AgentEvent>;

pub(crate) const MAILBOX_CAPACITY: usize = 256;

/// Capacity of the bounded event stream handed to consumers of a turn.
pub(crate) const EVENT_STREAM_CAPACITY: usize = 64;

/// Cloneable handle that only holds a bounded command channel. The actor task
/// owns `AgentState` exclusively.
#[derive(Clone)]
pub struct AgentHandle {
    pub(crate) commands: mpsc::Sender<AgentCommand>,
}

/// Structured error for mailbox saturation or actor shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentActorError {
    #[error("agent mailbox is full")]
    MailboxFull,
    #[error("agent actor is closed")]
    Closed,
}

impl From<AgentActorError> for AgentQueueError {
    fn from(error: AgentActorError) -> Self {
        match error {
            AgentActorError::MailboxFull => Self::MailboxFull,
            AgentActorError::Closed => Self::ActorClosed,
        }
    }
}

/// Commands accepted by the agent actor. Every mutating or querying command
/// carries a `oneshot` reply so the caller gets structured success or failure.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AgentCommand {
    Prompt {
        text: String,
        reply: oneshot::Sender<Result<AgentEventStream, AgentAdmissionError>>,
    },
    Run {
        reply: oneshot::Sender<Result<AgentEventStream, AgentAdmissionError>>,
    },
    Steer {
        text: String,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    SteerContent {
        content: Vec<ContentBlock>,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    FollowUp {
        text: String,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    FollowUpContent {
        content: Vec<ContentBlock>,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    Interject {
        text: String,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    InterjectContent {
        content: Vec<ContentBlock>,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    Abort {
        reply: oneshot::Sender<()>,
    },
    ClearQueues {
        reply: oneshot::Sender<()>,
    },
    EditQueueEntry {
        entry_id: String,
        expected_version: u32,
        new_message: AgentMessage,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    RemoveQueueEntry {
        entry_id: String,
        expected_version: u32,
        reply: oneshot::Sender<Result<(), AgentQueueError>>,
    },
    Messages {
        reply: oneshot::Sender<Vec<AgentMessage>>,
    },
    AddMessage {
        message: AgentMessage,
        reply: oneshot::Sender<()>,
    },
    ReplaceMessages {
        messages: Vec<AgentMessage>,
        reply: oneshot::Sender<()>,
    },
    SetToolRuntime {
        runtime: ToolRuntime,
        reply: oneshot::Sender<Result<(), ToolDefinitionError>>,
    },
    AddProviderTool {
        definition: ToolDefinition,
        reply: oneshot::Sender<Result<(), ToolDefinitionError>>,
    },
    SetResources {
        resources: AgentResources,
        reply: oneshot::Sender<()>,
    },
    ProviderRequestSnapshot {
        reply: oneshot::Sender<(Context, Option<StreamOptions>)>,
    },
    SetProviderRequestOverride {
        context: Box<Context>,
        stream_options: Option<StreamOptions>,
        reply: oneshot::Sender<()>,
    },
    BeforeProviderRequestHook {
        reply: oneshot::Sender<Option<BeforeProviderRequestHook>>,
    },
    SetBeforeProviderRequestHook {
        hook: Option<BeforeProviderRequestHook>,
        reply: oneshot::Sender<()>,
    },
    DrainSteeringQueue {
        reply: oneshot::Sender<Vec<AgentMessage>>,
    },
    DrainFollowUpQueue {
        reply: oneshot::Sender<Vec<AgentMessage>>,
    },
    Resources {
        reply: oneshot::Sender<AgentResources>,
    },
    Shutdown,
}

/// Allocates the bounded mailbox and returns the sender plus receiver halves.
pub(crate) fn mailbox() -> (mpsc::Sender<AgentCommand>, mpsc::Receiver<AgentCommand>) {
    mpsc::channel(MAILBOX_CAPACITY)
}

impl AgentHandle {
    /// Sends a command and awaits its structured reply. A failed send or a
    /// dropped reply (actor panicked or shut down) maps to `Closed`.
    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> AgentCommand,
    ) -> Result<T, AgentActorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let command = build(reply_tx);
        self.commands
            .send(command)
            .await
            .map_err(|_| AgentActorError::Closed)?;
        reply_rx.await.map_err(|_| AgentActorError::Closed)
    }

    /// Waits for the actor's reply, mapping a failed send or a dropped reply
    /// (actor panicked or shut down) to `Closed`.
    async fn await_reply<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> AgentCommand,
    ) -> Result<T, AgentQueueError> {
        self.request(build).await.map_err(AgentQueueError::from)
    }

    pub(crate) async fn try_prompt(
        &self,
        text: String,
    ) -> Result<AgentEventStream, AgentAdmissionError> {
        self.request(|reply| AgentCommand::Prompt { text, reply })
            .await
            .map_err(|_| AgentAdmissionError::Busy {
                operation: "prompt",
            })?
    }

    pub(crate) async fn try_run(&self) -> Result<AgentEventStream, AgentAdmissionError> {
        self.request(|reply| AgentCommand::Run { reply })
            .await
            .map_err(|_| AgentAdmissionError::Busy { operation: "run" })?
    }

    pub(crate) async fn steer(&self, text: String) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::Steer { text, reply })
            .await?
    }

    pub(crate) async fn steer_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::SteerContent { content, reply })
            .await?
    }

    pub(crate) async fn follow_up(&self, text: String) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::FollowUp { text, reply })
            .await?
    }

    pub(crate) async fn follow_up_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::FollowUpContent { content, reply })
            .await?
    }

    pub(crate) async fn interject(&self, text: String) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::Interject { text, reply })
            .await?
    }

    pub(crate) async fn interject_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> Result<(), AgentQueueError> {
        self.await_reply(|reply| AgentCommand::InterjectContent { content, reply })
            .await?
    }

    pub(crate) fn abort(&self) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self
            .commands
            .try_send(AgentCommand::Abort { reply: reply_tx });
    }

    /// Requests a graceful actor shutdown: any in-flight turn is committed
    /// and the actor task ends. All later commands fail with `Closed`.
    pub(crate) fn shutdown(&self) {
        let _ = self.commands.try_send(AgentCommand::Shutdown);
    }

    pub(crate) fn clear_queues(&self) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self
            .commands
            .try_send(AgentCommand::ClearQueues { reply: reply_tx });
    }

    pub(crate) async fn edit_queue_entry(
        &self,
        entry_id: String,
        expected_version: u32,
        new_message: AgentMessage,
    ) -> Result<Result<(), AgentQueueError>, AgentActorError> {
        self.request(|reply| AgentCommand::EditQueueEntry {
            entry_id,
            expected_version,
            new_message,
            reply,
        })
        .await
    }

    pub(crate) async fn remove_queue_entry(
        &self,
        entry_id: String,
        expected_version: u32,
    ) -> Result<Result<(), AgentQueueError>, AgentActorError> {
        self.request(|reply| AgentCommand::RemoveQueueEntry {
            entry_id,
            expected_version,
            reply,
        })
        .await
    }

    pub(crate) async fn messages(&self) -> Result<Vec<AgentMessage>, AgentActorError> {
        self.request(|reply| AgentCommand::Messages { reply }).await
    }

    pub(crate) fn add_message(&self, message: AgentMessage) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self.commands.try_send(AgentCommand::AddMessage {
            message,
            reply: reply_tx,
        });
    }

    pub(crate) fn replace_messages(&self, messages: Vec<AgentMessage>) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self.commands.try_send(AgentCommand::ReplaceMessages {
            messages,
            reply: reply_tx,
        });
    }

    pub(crate) async fn set_tool_runtime(
        &self,
        runtime: ToolRuntime,
    ) -> Result<Result<(), ToolDefinitionError>, AgentActorError> {
        self.request(|reply| AgentCommand::SetToolRuntime { runtime, reply })
            .await
    }

    pub(crate) async fn add_provider_tool(
        &self,
        definition: ToolDefinition,
    ) -> Result<Result<(), ToolDefinitionError>, AgentActorError> {
        self.request(|reply| AgentCommand::AddProviderTool { definition, reply })
            .await
    }

    pub(crate) fn set_resources(&self, resources: AgentResources) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self.commands.try_send(AgentCommand::SetResources {
            resources,
            reply: reply_tx,
        });
    }

    pub(crate) async fn provider_request_snapshot(
        &self,
    ) -> Result<(Context, Option<StreamOptions>), AgentActorError> {
        self.request(|reply| AgentCommand::ProviderRequestSnapshot { reply })
            .await
    }

    pub(crate) fn set_provider_request_override(
        &self,
        context: Context,
        stream_options: Option<StreamOptions>,
    ) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self
            .commands
            .try_send(AgentCommand::SetProviderRequestOverride {
                context: Box::new(context),
                stream_options,
                reply: reply_tx,
            });
    }

    pub(crate) async fn before_provider_request_hook(
        &self,
    ) -> Result<Option<BeforeProviderRequestHook>, AgentActorError> {
        self.request(|reply| AgentCommand::BeforeProviderRequestHook { reply })
            .await
    }

    pub(crate) fn set_before_provider_request_hook(&self, hook: Option<BeforeProviderRequestHook>) {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self
            .commands
            .try_send(AgentCommand::SetBeforeProviderRequestHook {
                hook,
                reply: reply_tx,
            });
    }

    pub(crate) async fn drain_steering_queue(&self) -> Result<Vec<AgentMessage>, AgentActorError> {
        self.request(|reply| AgentCommand::DrainSteeringQueue { reply })
            .await
    }

    pub(crate) async fn drain_follow_up_queue(&self) -> Result<Vec<AgentMessage>, AgentActorError> {
        self.request(|reply| AgentCommand::DrainFollowUpQueue { reply })
            .await
    }

    pub(crate) async fn resources(&self) -> Result<AgentResources, AgentActorError> {
        self.request(|reply| AgentCommand::Resources { reply })
            .await
    }
}

#[cfg(test)]
mod mailbox_tests {
    use super::*;

    #[tokio::test]
    async fn steer_returns_actor_closed_after_shutdown() {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        let handle = AgentHandle { commands };
        let error = handle
            .steer("blocked".into())
            .await
            .expect_err("actor is closed");
        assert_eq!(error, AgentQueueError::ActorClosed);
    }

    #[tokio::test]
    async fn steer_surfaces_enqueue_rejection_from_the_actor() {
        // The actor rejects the steer with a structured queue error, which
        // must reach the caller instead of being silently dropped.
        let (commands, mut receiver) = mpsc::channel(8);
        let handle = AgentHandle { commands };
        let actor = tokio::spawn(async move {
            let command = receiver.recv().await.expect("steer command arrives");
            match command {
                AgentCommand::Steer { reply, .. } => {
                    let _ = reply.send(Err(AgentQueueError::NotFound {
                        entry_id: "simulated".into(),
                    }));
                }
                _ => panic!("unexpected command"),
            }
        });
        let error = handle
            .steer("blocked".into())
            .await
            .expect_err("actor rejection must surface");
        assert_eq!(
            error,
            AgentQueueError::NotFound {
                entry_id: "simulated".into()
            }
        );
        actor.await.expect("actor task completes");
    }

    #[tokio::test]
    async fn shutdown_command_is_accepted() {
        let (commands, _receiver) = mpsc::channel(2);
        let handle = AgentHandle { commands };
        handle.shutdown();
        assert!(
            handle
                .commands
                .try_send(AgentCommand::ClearQueues {
                    reply: oneshot::channel().0,
                })
                .is_ok()
        );
    }
}
