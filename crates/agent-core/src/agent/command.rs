// TODO(ARC-D500, Phase 5): actor task + Agent migration in progress; remove allow once wired
#![allow(dead_code)]

use ai_protocol::api::conversation::{ContentBlock, Context};
use ai_protocol::api::stream::StreamOptions;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::{ToolDefinition, ToolDefinitionError};
use tool_runtime::api::ToolRuntime;

use crate::agent::AgentAdmissionError;
use crate::agent::types::{AgentEvent, AgentMessage, AgentQueueError, AgentResources};
use crate::hooks::BeforeProviderRequestHook;

/// Bounded event stream consumed by callers of `AgentHandle::prompt` / `run`.
pub type AgentEventStream = mpsc::Receiver<AgentEvent>;

pub(crate) const MAILBOX_CAPACITY: usize = 256;

/// Cloneable handle that only holds a bounded command channel and a shared
/// cancellation token. The actor task owns `AgentState` exclusively.
#[derive(Clone)]
pub struct AgentHandle {
    pub(crate) commands: mpsc::Sender<AgentCommand>,
    pub(crate) cancel_token: CancellationToken,
}

/// Structured error for mailbox saturation or actor shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentActorError {
    #[error("agent mailbox is full")]
    MailboxFull,
    #[error("agent actor is closed")]
    Closed,
}

impl AgentActorError {
    pub(crate) fn from_send<T>(error: mpsc::error::TrySendError<T>) -> Self {
        match error {
            mpsc::error::TrySendError::Full(_) => Self::MailboxFull,
            mpsc::error::TrySendError::Closed(_) => Self::Closed,
        }
    }
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
    Abort {
        reply: oneshot::Sender<()>,
    },
    ClearQueues {
        reply: oneshot::Sender<()>,
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
    Shutdown,
}

/// Allocates the bounded mailbox and returns the sender plus receiver halves.
pub(crate) fn mailbox() -> (mpsc::Sender<AgentCommand>, mpsc::Receiver<AgentCommand>) {
    mpsc::channel(MAILBOX_CAPACITY)
}
