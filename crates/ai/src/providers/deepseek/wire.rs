use crate::protocol::ResponsesTextFormat;
use serde::Serialize;

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
    pub format: ResponsesTextFormat,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseReasoning {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponseInputItem {
    Known(ResponseKnownInputItem),
    /// DeepSeek requires completed server-side items, notably
    /// `web_search_call`, to be passed back as-is on the next stateless turn.
    Provider(serde_json::Value),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseKnownInputItem {
    #[serde(rename = "message")]
    Message {
        role: String,
        content: Vec<ResponseMessageContent>,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        summary: Vec<serde_json::Value>,
        content: Vec<ResponseReasoningContent>,
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
pub struct ResponseMessageContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseReasoningContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
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
