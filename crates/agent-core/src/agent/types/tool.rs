use std::sync::Arc;

use ai_protocol::api::conversation::ContentBlock;
use tokio_util::sync::CancellationToken;
use tool_contract::api::output::{ToolError, ToolErrorKind, ToolOutput, ToolProgress};

#[derive(Debug, Clone)]
pub struct AgentToolOutput {
    pub content: Vec<ContentBlock>,
    pub details: Option<serde_json::Value>,
}

impl AgentToolOutput {
    pub fn new(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl From<ToolOutput> for AgentToolOutput {
    fn from(output: ToolOutput) -> Self {
        let result = AgentToolResult::from(output);
        Self {
            content: result.content,
            details: result.details,
        }
    }
}

impl From<Vec<ContentBlock>> for AgentToolOutput {
    fn from(content: Vec<ContentBlock>) -> Self {
        Self::new(content)
    }
}

impl From<ToolProgress> for AgentToolOutput {
    fn from(progress: ToolProgress) -> Self {
        crate::agent::tool_adapter::progress_to_agent_output(progress)
    }
}

#[derive(Debug, Clone)]
pub struct AgentToolResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    pub terminate: bool,
    pub details: Option<serde_json::Value>,
}

impl AgentToolResult {
    pub fn ok(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            is_error: false,
            terminate: false,
            details: None,
        }
    }

    pub fn from_output(output: AgentToolOutput) -> Self {
        Self {
            content: output.content,
            is_error: false,
            terminate: false,
            details: output.details,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        ToolError::new(ToolErrorKind::Execution, message).into()
    }
}

impl From<ToolOutput> for AgentToolResult {
    fn from(output: ToolOutput) -> Self {
        crate::agent::tool_adapter::output_to_agent_result(output)
    }
}

impl From<ToolError> for AgentToolResult {
    fn from(error: ToolError) -> Self {
        crate::agent::tool_adapter::error_to_agent_result(error)
    }
}

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    scope_id: Option<Arc<str>>,
    turn: u32,
    tool_call_id: Arc<str>,
    tool_name: Arc<str>,
    cancel_token: CancellationToken,
}

impl ToolExecutionContext {
    pub fn new(
        scope_id: Option<impl Into<Arc<str>>>,
        turn: u32,
        tool_call_id: impl Into<Arc<str>>,
        tool_name: impl Into<Arc<str>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            scope_id: scope_id.map(Into::into),
            turn,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            cancel_token,
        }
    }

    pub fn standalone(tool_name: impl Into<Arc<str>>) -> Self {
        Self::new(
            None::<Arc<str>>,
            0,
            Arc::<str>::from("direct"),
            tool_name,
            CancellationToken::new(),
        )
    }

    pub fn scope_id(&self) -> Option<&str> {
        self.scope_id.as_deref()
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }
}

pub type ToolUpdateCallback = Arc<dyn Fn(AgentToolOutput) + Send + Sync>;
