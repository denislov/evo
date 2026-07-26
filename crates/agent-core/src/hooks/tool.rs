use std::sync::Arc;

use ai::api::conversation::{AssistantMessage, ContentBlock};
use serde_json::Value;

use super::HookFuture;
use crate::agent::types::{AgentMessage, AgentToolResult, ToolExecutionContext};

pub type BeforeToolCallHook =
    Arc<dyn Fn(BeforeToolCallContext) -> HookFuture<Option<BeforeToolCallResult>> + Send + Sync>;
pub type AfterToolCallHook =
    Arc<dyn Fn(AfterToolCallContext) -> HookFuture<Option<AfterToolCallResult>> + Send + Sync>;

#[derive(Clone)]
pub struct BeforeToolCallContext {
    pub execution_context: ToolExecutionContext,
    /// Shared immutable turn snapshot. Tool hooks must not pay one full
    /// assistant/history clone per tool call.
    pub assistant_message: Arc<AssistantMessage>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub messages: Arc<[AgentMessage]>,
}

#[derive(Clone)]
pub struct BeforeToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
}

#[derive(Clone)]
pub struct AfterToolCallContext {
    /// Shared immutable turn snapshot.
    pub assistant_message: Arc<AssistantMessage>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub result: AgentToolResult,
    pub messages: Arc<[AgentMessage]>,
}

#[derive(Clone, Default)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<ContentBlock>>,
    pub is_error: Option<bool>,
    pub terminate: Option<bool>,
}
