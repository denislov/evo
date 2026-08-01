//! Shared Responses wire vocabulary. Provider dialects may use narrower
//! request types while sharing the response/event schema.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ResponseCreateRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    #[serde(rename = "max_output_tokens", skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(rename = "top_p", skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(rename = "top_logprobs", skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u8>,
    #[serde(rename = "tool_choice", skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(rename = "prompt_cache_key", skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponseReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<ResponseText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseText {
    pub format: crate::protocol::ResponsesTextFormat,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseReasoning {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponseInputItem {
    Known(ResponseKnownInputItem),
    Provider(serde_json::Value),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseKnownInputItem {
    #[serde(rename = "message")]
    Message {
        role: String,
        content: serde_json::Value,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        summary: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(rename = "encrypted_content", skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput { call_id: String, output: String },
    #[serde(rename = "custom_tool_call")]
    CustomToolCall {
        call_id: String,
        name: String,
        input: String,
    },
    #[serde(rename = "custom_tool_call_output")]
    CustomToolCallOutput { call_id: String, output: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseTool {
    #[serde(rename = "function")]
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        parameters: serde_json::Value,
    },
    #[serde(rename = "web_search")]
    WebSearch,
    #[serde(rename = "custom")]
    Custom {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

// ── SSE event types ────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ResponseStreamEvent {
    ResponseCreated {
        response: ResponseInfo,
    },
    OutputItemAdded {
        item: OutputItem,
    },
    ContentPartAdded {
        item_id: Option<String>,
        part: ContentPart,
    },
    OutputTextDelta {
        item_id: Option<String>,
        delta: String,
    },
    ReasoningTextDelta {
        item_id: Option<String>,
        delta: String,
    },
    ReasoningTextDone {
        item_id: Option<String>,
        text: Option<String>,
    },
    FunctionCallArgumentsDelta {
        item_id: Option<String>,
        delta: String,
    },
    CustomToolCallInputDelta {
        item_id: Option<String>,
        delta: String,
    },
    CustomToolCallInputDone {
        item_id: Option<String>,
        input: Option<String>,
    },
    WebSearchCallStatus {
        item_id: Option<String>,
        status: String,
    },
    OutputItemDone {
        item: OutputItem,
    },
    ResponseCompleted {
        response: ResponseInfo,
    },
    ResponseFailed {
        response: ResponseInfo,
    },
    ResponseIncomplete {
        response: ResponseInfo,
    },
    ResponseCancelled {
        response: ResponseInfo,
    },
    Error {
        error: ResponseError,
    },
    Bookkeeping,
    Unknown {
        event_type: String,
        raw: serde_json::Value,
    },
}

impl ResponseStreamEvent {
    pub fn parse(data: &str) -> Result<Self, String> {
        let raw: serde_json::Value =
            serde_json::from_str(data).map_err(|error| format!("invalid JSON: {error}"))?;
        let event_type = raw
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "event is missing string field `type`".to_string())?;

        match event_type {
            "response.created" => Ok(Self::ResponseCreated {
                response: field(&raw, "response")?,
            }),
            "response.output_item.added" => Ok(Self::OutputItemAdded {
                item: output_item(&raw)?,
            }),
            "response.content_part.added" => Ok(Self::ContentPartAdded {
                item_id: optional_string(&raw, "item_id"),
                part: field(&raw, "part")?,
            }),
            "response.output_text.delta" => Ok(Self::OutputTextDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.reasoning_text.delta" => Ok(Self::ReasoningTextDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.reasoning_text.done" => Ok(Self::ReasoningTextDone {
                item_id: optional_string(&raw, "item_id"),
                text: optional_string(&raw, "text"),
            }),
            "response.function_call_arguments.delta" => Ok(Self::FunctionCallArgumentsDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.custom_tool_call_input.delta" => Ok(Self::CustomToolCallInputDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.custom_tool_call_input.done" => Ok(Self::CustomToolCallInputDone {
                item_id: optional_string(&raw, "item_id"),
                input: optional_string(&raw, "input"),
            }),
            "response.web_search_call.in_progress" => Ok(Self::WebSearchCallStatus {
                item_id: optional_string(&raw, "item_id"),
                status: "in_progress".into(),
            }),
            "response.web_search_call.searching" => Ok(Self::WebSearchCallStatus {
                item_id: optional_string(&raw, "item_id"),
                status: "searching".into(),
            }),
            "response.web_search_call.completed" => Ok(Self::WebSearchCallStatus {
                item_id: optional_string(&raw, "item_id"),
                status: "completed".into(),
            }),
            "response.output_item.done" => Ok(Self::OutputItemDone {
                item: output_item(&raw)?,
            }),
            "response.completed" => Ok(Self::ResponseCompleted {
                response: field(&raw, "response")?,
            }),
            "response.failed" => Ok(Self::ResponseFailed {
                response: field(&raw, "response")?,
            }),
            "response.incomplete" => Ok(Self::ResponseIncomplete {
                response: field(&raw, "response")?,
            }),
            "response.cancelled" | "response.canceled" => Ok(Self::ResponseCancelled {
                response: field(&raw, "response")?,
            }),
            "error" => Ok(Self::Error {
                error: field(&raw, "error")?,
            }),
            "response.in_progress"
            | "response.queued"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.function_call_arguments.done"
            | "response.reasoning.delta"
            | "response.reasoning.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done" => Ok(Self::Bookkeeping),
            _ => Ok(Self::Unknown {
                event_type: event_type.to_string(),
                raw,
            }),
        }
    }
}

fn output_item(raw: &serde_json::Value) -> Result<OutputItem, String> {
    let value = raw
        .get("item")
        .cloned()
        .ok_or_else(|| "event is missing field `item`".to_string())?;
    let mut item: OutputItem = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid `item` field: {error}"))?;
    item.raw = value;
    Ok(item)
}

fn field<T: serde::de::DeserializeOwned>(raw: &serde_json::Value, name: &str) -> Result<T, String> {
    serde_json::from_value(
        raw.get(name)
            .cloned()
            .ok_or_else(|| format!("event is missing field `{name}`"))?,
    )
    .map_err(|error| format!("invalid `{name}` field: {error}"))
}

fn optional_string(raw: &serde_json::Value, name: &str) -> Option<String> {
    raw.get(name)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseInfo {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub usage: Option<ResponseUsage>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(rename = "type", default)]
    pub error_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, rename = "input_tokens_details")]
    pub input_tokens_details: Option<InputTokensDetails>,
    #[serde(default, rename = "output_tokens_details")]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InputTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub encrypted_content: Option<String>,
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default)]
    pub text: Option<String>,
}
