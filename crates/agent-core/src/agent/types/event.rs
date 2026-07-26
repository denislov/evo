use std::pin::Pin;

use ai::api::conversation::{AssistantMessage, Context};
use ai::api::model::Model;
use ai::api::stream::{AssistantMessageEvent, StreamOptions};
use futures::Stream;

use super::{AgentToolOutput, AgentToolResult};

// ── Provider request snapshots ──────────────────────────

#[derive(Debug, Clone)]
pub struct ProviderRequestSnapshot {
    pub model: Model,
    pub context: Context,
    pub stream_options: StreamOptions,
}

// ── AgentEvent ─────────────────────────────────────

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStart {
        turn: u32,
    },
    BeforeProviderRequest {
        request: ProviderRequestSnapshot,
    },
    LlmEvent(AssistantMessageEvent),
    ToolCallStart {
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ToolCallUpdate {
        tool_call_id: String,
        tool_name: String,
        update: AgentToolOutput,
    },
    ToolCallEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
    },
    AgentDone {
        message: AssistantMessage,
    },
    AgentError {
        error: String,
    },
    SessionCompacted {
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
        details: Option<serde_json::Value>,
    },
}

// ── AgentStream ────────────────────────────────────

pub type AgentStream = Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;
