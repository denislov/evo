use ai_protocol::api::conversation::{AssistantMessage, ContentBlock};

// ── AgentMessage ───────────────────────────────────

#[derive(Debug, Clone)]
pub enum AgentMessage {
    UserText {
        message_id: String,
        text: String,
    },
    Assistant {
        message_id: String,
        message: AssistantMessage,
    },
    ToolResult {
        message_id: String,
        tool_call_id: String,
        tool_name: String,
        is_error: bool,
        content: Vec<ContentBlock>,
    },
    SystemPrompt {
        message_id: String,
        text: String,
    },
    CompactionSummary {
        message_id: String,
        summary: String,
        tokens_before: u32,
    },
    BashExecution {
        message_id: String,
        command: String,
        output: String,
        exit_code: Option<i32>,
        cancelled: bool,
        truncated: bool,
        full_output_path: Option<String>,
        exclude_from_context: bool,
        timestamp: u64,
    },
    Custom {
        message_id: String,
        custom_type: String,
        content: Vec<ContentBlock>,
        display: bool,
        details: Option<serde_json::Value>,
        timestamp: u64,
    },
    BranchSummary {
        message_id: String,
        summary: String,
        from_id: String,
        timestamp: u64,
    },
}

impl AgentMessage {
    pub(crate) fn message_id(&self) -> &str {
        match self {
            Self::UserText { message_id, .. }
            | Self::Assistant { message_id, .. }
            | Self::ToolResult { message_id, .. }
            | Self::SystemPrompt { message_id, .. }
            | Self::CompactionSummary { message_id, .. }
            | Self::BashExecution { message_id, .. }
            | Self::Custom { message_id, .. }
            | Self::BranchSummary { message_id, .. } => message_id,
        }
    }

    pub(crate) fn set_message_id(&mut self, message_id: String) {
        match self {
            Self::UserText { message_id: id, .. }
            | Self::Assistant { message_id: id, .. }
            | Self::ToolResult { message_id: id, .. }
            | Self::SystemPrompt { message_id: id, .. }
            | Self::CompactionSummary { message_id: id, .. }
            | Self::BashExecution { message_id: id, .. }
            | Self::Custom { message_id: id, .. }
            | Self::BranchSummary { message_id: id, .. } => *id = message_id,
        }
    }
}
