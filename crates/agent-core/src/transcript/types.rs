use ai::api::conversation::{ContentBlock, StopReason};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl SessionEntry {
    pub fn message(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        message: StoredAgentMessage,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert(
            "message".into(),
            serde_json::to_value(message).expect("stored message serializes"),
        );
        Self {
            entry_type: "message".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn session_info(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        name: String,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert("name".into(), Value::String(name));
        Self {
            entry_type: "session_info".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn field(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role")]
pub enum StoredAgentMessage {
    #[serde(rename = "user")]
    User {
        content: Vec<ContentBlock>,
        timestamp: u64,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<ContentBlock>,
        api: String,
        provider: String,
        model: String,
        #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
        response_model: Option<String>,
        #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        usage: StoredUsage,
        #[serde(rename = "stopReason")]
        stop_reason: StopReason,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<ContentBlock>,
        #[serde(rename = "isError")]
        is_error: bool,
        timestamp: u64,
    },
    #[serde(rename = "bashExecution")]
    BashExecution {
        command: String,
        output: String,
        #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        cancelled: bool,
        truncated: bool,
        #[serde(rename = "fullOutputPath", skip_serializing_if = "Option::is_none")]
        full_output_path: Option<String>,
        #[serde(rename = "excludeFromContext", skip_serializing_if = "Option::is_none")]
        exclude_from_context: Option<bool>,
        timestamp: u64,
    },
    #[serde(rename = "custom")]
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        content: Vec<ContentBlock>,
        display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        timestamp: u64,
    },
    #[serde(rename = "branchSummary")]
    BranchSummary {
        summary: String,
        #[serde(rename = "fromId")]
        from_id: String,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredUsageCost {
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub known: bool,
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
}

impl Default for StoredUsageCost {
    fn default() -> Self {
        Self {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StoredUsage {
    pub input: u32,
    pub output: u32,
    #[serde(rename = "cacheRead")]
    pub cache_read: u32,
    #[serde(rename = "cacheWrite")]
    pub cache_write: u32,
    pub total: u32,
    pub cost: StoredUsageCost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: String,
}

/// A node in the session tree, built from a `SessionEntry` with resolved
/// label information and child nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
}
