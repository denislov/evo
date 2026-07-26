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
    #[serde(rename = "tool_choice", skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(rename = "prompt_cache_key", skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponseReasoning>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseReasoning {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseInputItem {
    #[serde(rename = "message")]
    Message {
        role: String,
        content: serde_json::Value,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput { call_id: String, output: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
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
    FunctionCallArgumentsDelta {
        item_id: Option<String>,
        delta: String,
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
                item: field(&raw, "item")?,
            }),
            "response.content_part.added" => Ok(Self::ContentPartAdded {
                item_id: optional_string(&raw, "item_id"),
                part: field(&raw, "part")?,
            }),
            "response.output_text.delta" => Ok(Self::OutputTextDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.function_call_arguments.delta" => Ok(Self::FunctionCallArgumentsDelta {
                item_id: optional_string(&raw, "item_id"),
                delta: field(&raw, "delta")?,
            }),
            "response.output_item.done" => Ok(Self::OutputItemDone {
                item: field(&raw, "item")?,
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
            | "response.function_call_arguments.done" => Ok(Self::Bookkeeping),
            _ => Ok(Self::Unknown {
                event_type: event_type.to_string(),
                raw,
            }),
        }
    }
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default)]
    pub text: Option<String>,
}
