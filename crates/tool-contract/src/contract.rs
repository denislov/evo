use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

const MAX_TOOL_ID_BYTES: usize = 64;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 1_024;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolId(String);

impl ToolId {
    pub fn new(value: impl Into<String>) -> Result<Self, ToolDefinitionError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_TOOL_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ToolDefinitionError::new(
                "id",
                "tool id must be 1-64 ASCII alphanumeric, underscore, or hyphen bytes",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ToolId {
    type Err = ToolDefinitionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ToolId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Function,
    Custom,
    WebSearch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

impl fmt::Display for ToolExecutionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
        })
    }
}

impl FromStr for ToolExecutionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "sequential" => Ok(Self::Sequential),
            "parallel" => Ok(Self::Parallel),
            _ => Err(format!("unknown tool execution mode: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ToolBehaviorVersion(u32);

impl ToolBehaviorVersion {
    pub const V1: Self = Self(1);

    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for ToolBehaviorVersion {
    fn default() -> Self {
        Self::V1
    }
}

impl<'de> Deserialize<'de> for ToolBehaviorVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?)
            .ok_or_else(|| serde::de::Error::custom("tool behavior version must be non-zero"))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationRisk {
    #[default]
    None,
    WorkspaceLocalReadOnly,
    SideEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapabilities {
    pub read_only: bool,
    pub execution: ToolExecutionMode,
    pub cancel: bool,
    pub timeout: bool,
    pub streaming: bool,
    pub provider_executed: bool,
}

impl Default for ToolCapabilities {
    fn default() -> Self {
        Self {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequirement {
    pub tool: ToolId,
    pub minimum_behavior: ToolBehaviorVersion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub id: ToolId,
    pub kind: ToolKind,
    pub description: String,
    pub parameters: serde_json::Value,
    pub capabilities: ToolCapabilities,
    pub behavior: ToolBehaviorVersion,
    pub authorization_risk: AuthorizationRisk,
    pub requirements: Vec<ToolRequirement>,
}

impl ToolDefinition {
    pub fn validate(&self) -> Result<(), ToolDefinitionError> {
        if self.description.trim().is_empty()
            || self.description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || self.description.chars().count() > MAX_TOOL_DESCRIPTION_CHARS
        {
            return Err(ToolDefinitionError::new(
                "description",
                "tool description must be non-empty and at most 1024 characters/4096 bytes",
            ));
        }
        if matches!(self.kind, ToolKind::Custom | ToolKind::WebSearch) && !self.parameters.is_null()
        {
            return Err(ToolDefinitionError::new(
                "parameters",
                "custom and provider-executed tools must not declare a JSON schema",
            ));
        }
        if self.kind == ToolKind::Function {
            crate::schema::validate_tool_schema(&self.parameters)?;
        }
        if self.capabilities.provider_executed != (self.kind == ToolKind::WebSearch) {
            return Err(ToolDefinitionError::new(
                "capabilities",
                "provider_executed must match the provider-executed tool kind",
            ));
        }
        Ok(())
    }
}

pub trait ToolArgs: DeserializeOwned + JsonSchema + Send + Sync + 'static {}

impl<T> ToolArgs for T where T: DeserializeOwned + JsonSchema + Send + Sync + 'static {}

pub fn schema_for<T: ToolArgs>() -> Result<serde_json::Value, ToolDefinitionError> {
    crate::schema::schema_for::<T>()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Image { data: String, mime_type: String },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<ToolContent>,
    pub details: Option<serde_json::Value>,
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub content: Vec<ToolContent>,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    InvalidArguments,
    Unauthorized,
    Cancelled,
    Timeout,
    Unavailable,
    Execution,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl ToolError {
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid tool {field}: {message}")]
pub struct ToolDefinitionError {
    field: &'static str,
    message: String,
}

impl ToolDefinitionError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }

    pub const fn field(&self) -> &'static str {
        self.field
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, JsonSchema)]
    struct SearchArgs {
        query: String,
        limit: Option<u32>,
    }

    #[test]
    fn typed_args_generate_an_object_schema() {
        let schema = schema_for::<SearchArgs>().expect("schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        let args = SearchArgs {
            query: "needle".into(),
            limit: Some(10),
        };
        assert_eq!(args.query, "needle");
        assert_eq!(args.limit, Some(10));
    }

    #[test]
    fn provider_execution_is_explicit_and_consistent() {
        let definition = ToolDefinition {
            id: ToolId::new("web_search").unwrap(),
            kind: ToolKind::WebSearch,
            description: "Search the web".into(),
            parameters: serde_json::Value::Null,
            capabilities: ToolCapabilities {
                provider_executed: true,
                ..Default::default()
            },
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        };
        definition.validate().expect("valid definition");
    }

    #[test]
    fn invalid_value_objects_cannot_enter_through_serde() {
        assert!(serde_json::from_str::<ToolId>("\"has space\"").is_err());
        assert!(serde_json::from_str::<ToolBehaviorVersion>("0").is_err());
    }

    #[test]
    fn unsupported_schema_expansion_is_rejected() {
        let definition = ToolDefinition {
            id: ToolId::new("unsafe_schema").unwrap(),
            kind: ToolKind::Function,
            description: "Reject arbitrary schema keywords".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "unevaluatedProperties": false
            }),
            capabilities: ToolCapabilities::default(),
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::SideEffect,
            requirements: Vec::new(),
        };
        assert_eq!(definition.validate().unwrap_err().field(), "parameters");
    }
}
