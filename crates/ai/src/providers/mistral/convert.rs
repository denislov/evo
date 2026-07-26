use super::wire;
use crate::model::{Model, ModelInput};
use crate::protocol::{ContentBlock, Context, Message, StreamOptions, ThinkingConfig};

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::ChatCompletionRequest {
    let mut messages = Vec::new();
    if let Some(system_prompt) = &ctx.system_prompt {
        messages.push(wire::ChatMessage {
            role: "system".into(),
            content: Some(wire::ChatContent::Text(system_prompt.clone())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    messages.extend(
        ctx.messages
            .iter()
            .filter_map(|m| convert_message(m, model)),
    );

    let tools = ctx.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| wire::ChatTool {
                tool_type: "function".into(),
                function: wire::FunctionDef {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                    strict: false,
                },
            })
            .collect()
    });

    let thinking = opts.as_ref().and_then(|o| o.thinking.as_ref());
    let reasoning_effort = resolve_reasoning_effort(model, thinking);
    let prompt_mode = resolve_prompt_mode(model, thinking, reasoning_effort.as_deref());

    wire::ChatCompletionRequest {
        model: model.id.clone(),
        messages,
        stream: true,
        temperature: opts.as_ref().and_then(|o| o.temperature),
        max_tokens: opts
            .as_ref()
            .and_then(|o| o.max_tokens)
            .or(Some(model.max_tokens)),
        tools,
        tool_choice: opts.as_ref().and_then(|o| o.tool_choice.clone()),
        prompt_mode,
        reasoning_effort,
    }
}

fn convert_message(msg: &Message, model: &Model) -> Option<wire::ChatMessage> {
    match msg {
        Message::User { content } => Some(wire::ChatMessage {
            role: "user".into(),
            content: Some(convert_user_content(content, model_supports_images(model))),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }),
        Message::Assistant { content } => {
            let content_parts = convert_assistant_content(content);
            let tool_calls = convert_tool_calls(content);
            if content_parts.is_empty() && tool_calls.is_empty() {
                return None;
            }
            Some(wire::ChatMessage {
                role: "assistant".into(),
                content: if content_parts.is_empty() {
                    None
                } else {
                    Some(wire::ChatContent::Parts(content_parts))
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
                name: None,
            })
        }
        Message::ToolResult {
            tool_call_id,
            tool_name,
            is_error,
            content,
        } => Some(wire::ChatMessage {
            role: "tool".into(),
            content: Some(wire::ChatContent::Parts(vec![
                wire::ChatContentPart::Text {
                    text: build_tool_result_text(content, is_error.unwrap_or(false)),
                },
            ])),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.clone()),
            name: tool_name.clone(),
        }),
    }
}

fn convert_user_content(content: &[ContentBlock], supports_images: bool) -> wire::ChatContent {
    let has_image = content
        .iter()
        .any(|b| matches!(b, ContentBlock::Image { .. }));
    if !has_image {
        return wire::ChatContent::Text(content_to_text(content));
    }

    let mut parts = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text, .. } => {
                parts.push(wire::ChatContentPart::Text { text: text.clone() });
            }
            ContentBlock::Image { data, mime_type } if supports_images => {
                parts.push(wire::ChatContentPart::ImageUrl {
                    image_url: format!("data:{};base64,{}", mime_type, data),
                });
            }
            ContentBlock::Image { .. } => {}
            _ => {}
        }
    }
    if parts.is_empty() && has_image {
        return wire::ChatContent::Text("(image omitted: model does not support images)".into());
    }
    wire::ChatContent::Parts(parts)
}

fn convert_assistant_content(content: &[ContentBlock]) -> Vec<wire::ChatContentPart> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                Some(wire::ChatContentPart::Text { text: text.clone() })
            }
            ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                Some(wire::ChatContentPart::Thinking {
                    thinking: vec![wire::ThinkingPart::Text {
                        text: thinking.clone(),
                    }],
                })
            }
            _ => None,
        })
        .collect()
}

fn convert_tool_calls(content: &[ContentBlock]) -> Vec<wire::ToolCall> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(wire::ToolCall {
                id: normalize_tool_call_id(id),
                tool_type: "function".into(),
                function: wire::ToolCallFunction {
                    name: name.clone(),
                    arguments: arguments.to_string(),
                },
            }),
            _ => None,
        })
        .collect()
}

fn build_tool_result_text(content: &[ContentBlock], is_error: bool) -> String {
    let text = content_to_text(content);
    let trimmed = text.trim();
    let prefix = if is_error { "[tool error] " } else { "" };
    if trimmed.is_empty() {
        return if is_error {
            "[tool error] (no tool output)".into()
        } else {
            "(no tool output)".into()
        };
    }
    format!("{}{}", prefix, trimmed)
}

fn content_to_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn model_supports_images(model: &Model) -> bool {
    model.input.contains(&ModelInput::Image)
}

fn resolve_reasoning_effort(model: &Model, thinking: Option<&ThinkingConfig>) -> Option<String> {
    let thinking = thinking?;
    if !model.reasoning || !thinking.enabled || !uses_reasoning_effort(model) {
        return None;
    }
    Some(thinking.effort.clone().unwrap_or_else(|| "high".into()))
}

fn resolve_prompt_mode(
    model: &Model,
    thinking: Option<&ThinkingConfig>,
    reasoning_effort: Option<&str>,
) -> Option<String> {
    let thinking = thinking?;
    if model.reasoning && thinking.enabled && reasoning_effort.is_none() {
        return Some("reasoning".into());
    }
    None
}

fn uses_reasoning_effort(model: &Model) -> bool {
    matches!(
        model.id.as_str(),
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

fn normalize_tool_call_id(id: &str) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if normalized.len() == 9 {
        return normalized;
    }
    let seed = if normalized.is_empty() {
        id
    } else {
        &normalized
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:09x}", hash)[..9].to_string()
}
