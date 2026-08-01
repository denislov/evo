use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions};

pub fn build_request(
    _model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::GenerateContentRequest {
    let system_instruction = ctx.system_prompt.as_ref().map(|sp| wire::GeminiContent {
        role: "system".to_string(),
        parts: vec![wire::GeminiPart {
            text: Some(sp.clone()),
            inline_data: None,
            function_call: None,
            function_response: None,
        }],
    });

    let contents: Vec<wire::GeminiContent> =
        ctx.messages.iter().filter_map(convert_message).collect();

    let tools = ctx.tools.as_ref().map(|tools| {
        vec![wire::GeminiTool {
            function_declarations: tools
                .iter()
                .map(|t| wire::GeminiFunctionDeclaration {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                })
                .collect(),
        }]
    });

    let tool_config =
        opts.as_ref()
            .and_then(|o| o.tool_choice.clone())
            .map(|tc| wire::ToolConfig {
                function_calling_config: tc,
            });

    let max_tokens = opts.as_ref().and_then(|o| o.max_tokens);
    let temperature = opts.as_ref().and_then(|o| o.temperature);

    let thinking_config = opts.as_ref().and_then(|o| {
        o.thinking.as_ref().filter(|t| t.enabled).map(|t| {
            serde_json::json!({
                "thinkingBudget": t.budget_tokens.unwrap_or(2048),
            })
        })
    });

    wire::GenerateContentRequest {
        system_instruction,
        contents,
        tools,
        tool_config,
        generation_config: Some(wire::GenerationConfig {
            max_output_tokens: max_tokens,
            temperature,
            thinking_config,
        }),
    }
}

fn convert_message(msg: &Message) -> Option<wire::GeminiContent> {
    match msg {
        Message::User { content } => {
            let parts = convert_content(content);
            if parts.is_empty() {
                return None;
            }
            Some(wire::GeminiContent {
                role: "user".to_string(),
                parts,
            })
        }
        Message::Assistant { content } => {
            let parts = convert_content(content);
            if parts.is_empty() {
                return None;
            }
            Some(wire::GeminiContent {
                role: "model".to_string(),
                parts,
            })
        }
        Message::ToolResult {
            tool_call_id,
            tool_name,
            content,
            ..
        } => {
            let text = content_to_text(content);
            Some(wire::GeminiContent {
                role: "user".to_string(),
                parts: vec![wire::GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some(wire::FunctionResponse {
                        name: tool_name
                            .clone()
                            .or_else(|| derive_tool_name_from_id(tool_call_id))
                            .unwrap_or_default(),
                        response: serde_json::json!({ "text": text }),
                    }),
                }],
            })
        }
    }
}

fn convert_content(content: &[ContentBlock]) -> Vec<wire::GeminiPart> {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(wire::GeminiPart {
                text: Some(text.clone()),
                inline_data: None,
                function_call: None,
                function_response: None,
            }),
            ContentBlock::Image { data, mime_type } => Some(wire::GeminiPart {
                text: None,
                inline_data: Some(wire::InlineData {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                }),
                function_call: None,
                function_response: None,
            }),
            ContentBlock::ToolCall {
                id: _,
                name,
                arguments,
                ..
            } => Some(wire::GeminiPart {
                text: None,
                inline_data: None,
                function_call: Some(wire::FunctionCall {
                    name: name.clone(),
                    args: arguments.clone(),
                }),
                function_response: None,
            }),
            _ => None,
        })
        .collect()
}

fn content_to_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recover the function name from a synthetic tool-call id of the shape
/// `{name}-{serial}` when the explicit tool name is unavailable.
fn derive_tool_name_from_id(tool_call_id: &str) -> Option<String> {
    tool_call_id
        .rsplit_once('-')
        .filter(|(_, serial)| !serial.is_empty() && serial.bytes().all(|b| b.is_ascii_digit()))
        .map(|(name, _)| name.to_string())
        .filter(|name| !name.is_empty())
}
