use std::sync::Arc;

use ai::api::conversation::{AssistantMessage, Message};
use ai::api::model::Model;
use ai::api::stream::StreamOptions;

use super::{AfterToolCallHook, BeforeProviderRequestHook, BeforeToolCallHook, HookFuture};
use crate::agent::types::{AgentMessage, AgentResources, ThinkingLevel};

pub type ShouldStopAfterTurnHook =
    Arc<dyn Fn(ShouldStopAfterTurnContext) -> HookFuture<bool> + Send + Sync>;
pub type PrepareNextTurnHook =
    Arc<dyn Fn(PrepareNextTurnContext) -> HookFuture<Option<AgentLoopTurnUpdate>> + Send + Sync>;
pub type TransformContextHook =
    Arc<dyn Fn(Vec<AgentMessage>) -> HookFuture<Vec<AgentMessage>> + Send + Sync>;
pub type ConvertToLlmHook =
    Arc<dyn Fn(Vec<AgentMessage>, AgentResources) -> HookFuture<Vec<Message>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct AgentHooks {
    pub before_provider_request: Option<BeforeProviderRequestHook>,
    pub before_tool_call: Option<BeforeToolCallHook>,
    pub after_tool_call: Option<AfterToolCallHook>,
    pub should_stop_after_turn: Option<ShouldStopAfterTurnHook>,
    pub prepare_next_turn: Option<PrepareNextTurnHook>,
    pub transform_context: Option<TransformContextHook>,
    pub convert_to_llm: Option<ConvertToLlmHook>,
}

impl std::fmt::Debug for AgentHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHooks")
            .field(
                "before_provider_request",
                &self.before_provider_request.as_ref().map(|_| ".."),
            )
            .field(
                "before_tool_call",
                &self.before_tool_call.as_ref().map(|_| ".."),
            )
            .field(
                "after_tool_call",
                &self.after_tool_call.as_ref().map(|_| ".."),
            )
            .field(
                "should_stop_after_turn",
                &self.should_stop_after_turn.as_ref().map(|_| ".."),
            )
            .field(
                "prepare_next_turn",
                &self.prepare_next_turn.as_ref().map(|_| ".."),
            )
            .field(
                "transform_context",
                &self.transform_context.as_ref().map(|_| ".."),
            )
            .field(
                "convert_to_llm",
                &self.convert_to_llm.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl AgentHooks {
    pub fn is_empty(&self) -> bool {
        self.before_tool_call.is_none()
            && self.before_provider_request.is_none()
            && self.after_tool_call.is_none()
            && self.should_stop_after_turn.is_none()
            && self.prepare_next_turn.is_none()
            && self.transform_context.is_none()
            && self.convert_to_llm.is_none()
    }
}

#[derive(Clone)]
pub struct ShouldStopAfterTurnContext {
    pub messages: Vec<AgentMessage>,
    pub assistant_message: AssistantMessage,
}

#[derive(Clone)]
pub struct PrepareNextTurnContext {
    pub messages: Vec<AgentMessage>,
    pub turn: u32,
}

#[derive(Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub messages: Option<Vec<AgentMessage>>,
    pub thinking_level: Option<ThinkingLevel>,
    pub model: Option<Model>,
    pub stream_options: Option<StreamOptions>,
}
