use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::BTreeSet, fmt};

use ai::api::conversation::ContentBlock;
use tokio_util::sync::CancellationToken;

// ── ToolExecutionMode ──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

impl std::fmt::Display for ToolExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ToolExecutionMode::Sequential => "sequential",
            ToolExecutionMode::Parallel => "parallel",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for ToolExecutionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sequential" => Ok(ToolExecutionMode::Sequential),
            "parallel" => Ok(ToolExecutionMode::Parallel),
            _ => Err(format!("unknown tool execution mode: {}", s)),
        }
    }
}

// ── AgentToolResult ────────────────────────────────

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

impl From<Vec<ContentBlock>> for AgentToolOutput {
    fn from(content: Vec<ContentBlock>) -> Self {
        Self::new(content)
    }
}

// ── AgentToolResult ────────────────────────────────

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
        Self {
            content: vec![ContentBlock::Text {
                text: message.into(),
                text_signature: None,
            }],
            is_error: true,
            terminate: false,
            details: None,
        }
    }
}

// ── AgentTool ──────────────────────────────────────

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

pub type ToolFn = Arc<
    dyn Fn(
            ToolExecutionContext,
            serde_json::Value,
            Option<ToolUpdateCallback>,
        ) -> Pin<Box<dyn Future<Output = Result<AgentToolOutput, String>> + Send>>
        + Send
        + Sync,
>;
pub type ToolUpdateCallback = Arc<dyn Fn(AgentToolOutput) + Send + Sync>;
#[derive(Clone)]
pub struct AgentTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub execute: ToolFn,
    pub execution_mode: Option<ToolExecutionMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolDefinitionError {
    field: &'static str,
    message: String,
}

impl AgentToolDefinitionError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }

    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AgentToolDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid agent tool {}: {}", self.field, self.message)
    }
}

impl std::error::Error for AgentToolDefinitionError {}

const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 1_024;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 4_096;
const MAX_TOOL_SCHEMA_BYTES: usize = 32_768;
const MAX_TOOL_SCHEMA_DEPTH: usize = 12;
const MAX_TOOL_SCHEMA_NODES: usize = 512;
const MAX_TOOL_SCHEMA_PROPERTIES: usize = 256;
const MAX_PROPERTIES_PER_OBJECT: usize = 64;
const MAX_SCHEMA_DESCRIPTION_CHARS: usize = 1_024;
const MAX_SCHEMA_ENUM_VALUES: usize = 64;

#[derive(Default)]
struct ToolSchemaBudget {
    nodes: usize,
    properties: usize,
}

impl AgentTool {
    pub fn validate(&self) -> Result<(), AgentToolDefinitionError> {
        if self.name.is_empty()
            || self.name.len() > MAX_TOOL_NAME_BYTES
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(AgentToolDefinitionError::new(
                "name",
                "tool name must be 1-64 ASCII alphanumeric, underscore, or hyphen bytes",
            ));
        }
        if self.description.trim().is_empty()
            || self.description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || self.description.chars().count() > MAX_TOOL_DESCRIPTION_CHARS
        {
            return Err(AgentToolDefinitionError::new(
                "description",
                "tool description must be non-empty and at most 1024 characters/4096 bytes",
            ));
        }
        let serialized = serde_json::to_vec(&self.parameters).map_err(|error| {
            AgentToolDefinitionError::new(
                "parameters",
                format!("tool parameters cannot serialize: {error}"),
            )
        })?;
        if serialized.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err(AgentToolDefinitionError::new(
                "parameters",
                "tool parameters schema exceeds 32768 bytes",
            ));
        }
        let mut budget = ToolSchemaBudget::default();
        validate_tool_schema(&self.parameters, 0, true, &mut budget)?;
        Ok(())
    }

    pub fn validate_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<(), AgentToolArgumentError> {
        if tool_arguments_match_schema(&self.parameters, arguments) {
            Ok(())
        } else {
            Err(AgentToolArgumentError {
                message: format!(
                    "tool arguments do not match the registered schema for {}",
                    self.name
                ),
            })
        }
    }

    pub fn new_text<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        f: F,
    ) -> Self
    where
        F: Fn(ToolExecutionContext, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            execution_mode: None,
            execute: Arc::new(move |context, args, _on_update| {
                let fut = f(context, args);
                Box::pin(async move {
                    fut.await.map(|text| {
                        AgentToolOutput::new(vec![ContentBlock::Text {
                            text,
                            text_signature: None,
                        }])
                    })
                })
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolArgumentError {
    message: String,
}

impl AgentToolArgumentError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentToolArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentToolArgumentError {}

fn validate_tool_schema(
    value: &serde_json::Value,
    depth: usize,
    root: bool,
    budget: &mut ToolSchemaBudget,
) -> Result<(), AgentToolDefinitionError> {
    schema_require(
        depth <= MAX_TOOL_SCHEMA_DEPTH,
        "tool parameters schema exceeds maximum depth",
    )?;
    budget.nodes = budget
        .nodes
        .checked_add(1)
        .ok_or_else(|| schema_error("tool parameters schema node count overflow"))?;
    schema_require(
        budget.nodes <= MAX_TOOL_SCHEMA_NODES,
        "tool parameters schema contains too many nodes",
    )?;

    let object = value
        .as_object()
        .ok_or_else(|| schema_error("every tool parameters schema node must be an object"))?;
    const ALLOWED_KEYS: &[&str] = &[
        "type",
        "description",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "x-evo-authorization-risk",
    ];
    schema_require(
        object
            .keys()
            .all(|key| ALLOWED_KEYS.contains(&key.as_str())),
        "tool parameters schema contains an unsupported keyword",
    )?;

    let schema_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| schema_error("every tool parameters schema node requires a string type"))?;
    schema_require(
        matches!(
            schema_type,
            "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
        ),
        "tool parameters schema contains an unsupported type",
    )?;
    schema_require(
        !root || schema_type == "object",
        "tool parameters root type must be object",
    )?;

    if let Some(description) = object.get("description") {
        let description = description
            .as_str()
            .ok_or_else(|| schema_error("tool schema description must be a string"))?;
        schema_require(
            description.chars().count() <= MAX_SCHEMA_DESCRIPTION_CHARS,
            "tool schema description exceeds 1024 characters",
        )?;
    }
    if let Some(risk) = object.get("x-evo-authorization-risk") {
        schema_require(
            root && matches!(
                risk.as_str(),
                Some("workspace_local_read_only" | "side_effect")
            ),
            "x-evo-authorization-risk must be a supported root string",
        )?;
    }

    let properties = object.get("properties");
    let required = object.get("required");
    if schema_type == "object" {
        let properties = properties
            .map(|properties| {
                properties
                    .as_object()
                    .ok_or_else(|| schema_error("tool schema properties must be an object"))
            })
            .transpose()?;
        if let Some(properties) = properties {
            schema_require(
                properties.len() <= MAX_PROPERTIES_PER_OBJECT,
                "tool schema object contains more than 64 properties",
            )?;
            budget.properties = budget
                .properties
                .checked_add(properties.len())
                .ok_or_else(|| schema_error("tool parameters property count overflow"))?;
            schema_require(
                budget.properties <= MAX_TOOL_SCHEMA_PROPERTIES,
                "tool parameters schema contains more than 256 properties",
            )?;
            for (name, schema) in properties {
                schema_require(
                    !name.is_empty() && name.chars().count() <= 64,
                    "tool schema property name is empty or exceeds 64 characters",
                )?;
                validate_tool_schema(schema, depth + 1, false, budget)?;
            }
        }
        if let Some(required) = required {
            let required = required
                .as_array()
                .ok_or_else(|| schema_error("tool schema required must be an array"))?;
            let mut names = BTreeSet::new();
            for name in required {
                let name = name
                    .as_str()
                    .ok_or_else(|| schema_error("tool schema required entries must be strings"))?;
                schema_require(
                    names.insert(name),
                    "tool schema required entries must be unique",
                )?;
                schema_require(
                    properties.is_some_and(|properties| properties.contains_key(name)),
                    "tool schema required entry is not declared in properties",
                )?;
            }
        }
        if let Some(additional) = object.get("additionalProperties") {
            schema_require(
                additional.as_bool().is_some(),
                "tool schema additionalProperties must be boolean",
            )?;
        }
    } else {
        schema_require(
            properties.is_none()
                && required.is_none()
                && object.get("additionalProperties").is_none(),
            "non-object tool schema cannot declare object keywords",
        )?;
    }

    if schema_type == "array" {
        let items = object
            .get("items")
            .ok_or_else(|| schema_error("array tool schema requires items"))?;
        validate_tool_schema(items, depth + 1, false, budget)?;
    } else {
        schema_require(
            object.get("items").is_none(),
            "non-array tool schema cannot declare items",
        )?;
    }

    if let Some(values) = object.get("enum") {
        let values = values
            .as_array()
            .ok_or_else(|| schema_error("tool schema enum must be an array"))?;
        schema_require(
            !values.is_empty() && values.len() <= MAX_SCHEMA_ENUM_VALUES,
            "tool schema enum must contain 1-64 values",
        )?;
        schema_require(
            !matches!(schema_type, "object" | "array"),
            "object and array tool schemas cannot declare scalar enum values",
        )?;
        let mut unique_values = BTreeSet::new();
        for value in values {
            schema_require(
                !value.is_array() && !value.is_object(),
                "tool schema enum values must be scalar",
            )?;
            schema_require(
                enum_value_matches_type(value, schema_type),
                "tool schema enum value does not match the declared type",
            )?;
            schema_require(
                unique_values.insert(value.to_string()),
                "tool schema enum values must be unique",
            )?;
        }
    }

    schema_require(
        !(object.contains_key("minimum") || object.contains_key("maximum"))
            || matches!(schema_type, "number" | "integer"),
        "only number or integer tool schemas can declare numeric bounds",
    )?;
    schema_require(
        !(object.contains_key("minLength") || object.contains_key("maxLength"))
            || schema_type == "string",
        "only string tool schemas can declare length bounds",
    )?;
    schema_require(
        !(object.contains_key("minItems") || object.contains_key("maxItems"))
            || schema_type == "array",
        "only array tool schemas can declare item bounds",
    )?;
    validate_numeric_range(object, "minimum", "maximum")?;
    validate_unsigned_range(object, "minLength", "maxLength")?;
    validate_unsigned_range(object, "minItems", "maxItems")
}

pub fn tool_arguments_match_schema(schema: &serde_json::Value, value: &serde_json::Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return false;
    };
    let Some(schema_type) = schema.get("type").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let type_matches = match schema_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };
    if !type_matches {
        return false;
    }
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array)
        && !values.contains(value)
    {
        return false;
    }

    match schema_type {
        "object" => {
            let value = value.as_object().expect("object type was checked");
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            if schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|required| {
                    required
                        .iter()
                        .any(|name| name.as_str().is_none_or(|name| !value.contains_key(name)))
                })
            {
                return false;
            }
            let additional_allowed = schema
                .get("additionalProperties")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            for (name, property) in value {
                match properties.and_then(|properties| properties.get(name)) {
                    Some(property_schema)
                        if tool_arguments_match_schema(property_schema, property) => {}
                    Some(_) => return false,
                    None if !additional_allowed => return false,
                    None => {}
                }
            }
        }
        "array" => {
            let value = value.as_array().expect("array type was checked");
            let Some(items) = schema.get("items") else {
                return false;
            };
            if !value
                .iter()
                .all(|item| tool_arguments_match_schema(items, item))
            {
                return false;
            }
            let len = value.len() as u64;
            if schema
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|minimum| len < minimum)
                || schema
                    .get("maxItems")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|maximum| len > maximum)
            {
                return false;
            }
        }
        "string" => {
            let len = value
                .as_str()
                .expect("string type was checked")
                .chars()
                .count() as u64;
            if schema
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|minimum| len < minimum)
                || schema
                    .get("maxLength")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|maximum| len > maximum)
            {
                return false;
            }
        }
        "number" | "integer" => {
            let Some(number) = value.as_f64() else {
                return false;
            };
            if schema
                .get("minimum")
                .and_then(serde_json::Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || schema
                    .get("maximum")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|maximum| number > maximum)
            {
                return false;
            }
        }
        "boolean" | "null" => {}
        _ => return false,
    }
    true
}

fn enum_value_matches_type(value: &serde_json::Value, schema_type: &str) -> bool {
    match schema_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "object" | "array" => false,
        _ => false,
    }
}

fn validate_numeric_range(
    object: &serde_json::Map<String, serde_json::Value>,
    minimum: &str,
    maximum: &str,
) -> Result<(), AgentToolDefinitionError> {
    let minimum = object
        .get(minimum)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| schema_error("tool schema numeric bound must be finite"))
        })
        .transpose()?;
    let maximum = object
        .get(maximum)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| schema_error("tool schema numeric bound must be finite"))
        })
        .transpose()?;
    schema_require(
        minimum
            .zip(maximum)
            .is_none_or(|(minimum, maximum)| minimum <= maximum),
        "tool schema minimum cannot exceed maximum",
    )
}

fn validate_unsigned_range(
    object: &serde_json::Map<String, serde_json::Value>,
    minimum: &str,
    maximum: &str,
) -> Result<(), AgentToolDefinitionError> {
    let minimum = object
        .get(minimum)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| schema_error("tool schema size bound must be an unsigned integer"))
        })
        .transpose()?;
    let maximum = object
        .get(maximum)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| schema_error("tool schema size bound must be an unsigned integer"))
        })
        .transpose()?;
    schema_require(
        minimum
            .zip(maximum)
            .is_none_or(|(minimum, maximum)| minimum <= maximum),
        "tool schema minimum size cannot exceed maximum size",
    )
}

fn schema_require(condition: bool, message: &'static str) -> Result<(), AgentToolDefinitionError> {
    condition.then_some(()).ok_or_else(|| schema_error(message))
}

fn schema_error(message: impl Into<String>) -> AgentToolDefinitionError {
    AgentToolDefinitionError::new("parameters", message)
}
