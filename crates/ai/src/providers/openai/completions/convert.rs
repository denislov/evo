use super::wire;
use crate::compatibility::ThinkingFormat;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions, ToolKind};
use crate::providers::common::unsupported_tool_error;
use crate::providers::openai::common::{CompatFlags, resolve_completions_compat};

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> Result<wire::ChatCompletionRequest, String> {
    let compat = resolve_completions_compat(model);
    let mut messages = Vec::new();

    if let Some(sp) = &ctx.system_prompt {
        let role = if compat.supports_developer_role && model.reasoning {
            "developer"
        } else {
            "system"
        };
        messages.push(wire::ChatMessage {
            role: role.to_string(),
            content: wire::ChatContent::Text(sp.clone()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }

    messages.extend(
        ctx.messages
            .iter()
            .map(|m| convert_message(m, model.reasoning, &compat)),
    );

    let mut max_tokens: Option<u32> = None;
    let mut max_completion_tokens: Option<u32> = None;
    let tokens = opts
        .as_ref()
        .and_then(|o| o.max_tokens)
        .unwrap_or(model.max_tokens);
    match compat.max_tokens_field.as_str() {
        "max_tokens" => max_tokens = Some(tokens),
        _ => max_completion_tokens = Some(tokens),
    }

    let temperature = opts.as_ref().and_then(|options| {
        let deepseek_thinking_enabled = compat.thinking_format == Some(ThinkingFormat::DeepSeek)
            && options
                .thinking
                .as_ref()
                .is_some_and(|thinking| thinking.enabled);
        (!deepseek_thinking_enabled)
            .then_some(options.temperature)
            .flatten()
    });
    let tool_choice = opts.as_ref().and_then(|o| o.tool_choice.clone());

    let tools = ctx
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|t| match t.kind {
                    ToolKind::Function => Ok(wire::ChatTool {
                        tool_type: "function".to_string(),
                        function: wire::FunctionDef {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                            strict: compat.supports_strict_mode.then_some(true),
                        },
                    }),
                    // Chat-completions has no server-side tool execution.
                    ToolKind::WebSearch | ToolKind::Custom => {
                        Err(unsupported_tool_error("openai-completions", t))
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;

    let stream_options = if compat.supports_usage_in_streaming {
        Some(wire::StreamOptions {
            include_usage: true,
        })
    } else {
        None
    };
    let (thinking, reasoning_effort) = thinking_params(model, opts, &compat);

    Ok(wire::ChatCompletionRequest {
        model: model.id.clone(),
        messages,
        max_tokens,
        max_completion_tokens,
        temperature,
        tools,
        tool_choice,
        thinking,
        reasoning_effort,
        stream: true,
        stream_options,
    })
}

fn thinking_params(
    model: &Model,
    opts: &Option<StreamOptions>,
    compat: &CompatFlags,
) -> (Option<serde_json::Value>, Option<String>) {
    if !model.reasoning || compat.thinking_format != Some(ThinkingFormat::DeepSeek) {
        return (None, None);
    }

    let Some(thinking_config) = opts.as_ref().and_then(|opts| opts.thinking.as_ref()) else {
        return (None, None);
    };

    if !thinking_config.enabled {
        return (Some(serde_json::json!({ "type": "disabled" })), None);
    }

    let effort = thinking_config.effort.as_deref().and_then(|level| {
        model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.resolve(level))
    });

    (Some(serde_json::json!({ "type": "enabled" })), effort)
}

fn convert_message(
    msg: &Message,
    model_reasoning: bool,
    compat: &CompatFlags,
) -> wire::ChatMessage {
    match msg {
        Message::User { content } => wire::ChatMessage {
            role: "user".to_string(),
            content: convert_user_content(content),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message::Assistant { content } => {
            let mut text_parts = Vec::new();
            let mut thinking_parts = Vec::new();
            let mut tool_calls = Vec::new();
            for block in content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        text_parts.push(wire::ChatContentPart::Text { text: text.clone() });
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        if !thinking.is_empty() {
                            thinking_parts.push(thinking.clone());
                        }
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } => {
                        tool_calls.push(wire::ToolCall {
                            id: id.clone(),
                            tool_type: "function".to_string(),
                            function: wire::ToolCallFunction {
                                name: name.clone(),
                                arguments: arguments.to_string(),
                            },
                        });
                    }
                    _ => {}
                }
            }
            let content = if text_parts.is_empty() && !tool_calls.is_empty() {
                wire::ChatContent::Text(String::new())
            } else {
                wire::ChatContent::Parts(text_parts)
            };
            let reasoning_content =
                if model_reasoning && compat.requires_reasoning_content_on_assistant_messages {
                    Some(thinking_parts.join("\n"))
                } else {
                    None
                };
            wire::ChatMessage {
                role: "assistant".to_string(),
                content,
                name: None,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
                reasoning_content,
            }
        }
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => {
            let text = content_to_text(content);
            wire::ChatMessage {
                role: "tool".to_string(),
                content: wire::ChatContent::Text(text),
                name: None,
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                reasoning_content: None,
            }
        }
    }
}

fn convert_user_content(content: &[ContentBlock]) -> wire::ChatContent {
    let has_image = content
        .iter()
        .any(|b| matches!(b, ContentBlock::Image { .. }));
    if !has_image {
        let text = content_to_text(content);
        return wire::ChatContent::Text(text);
    }

    let parts: Vec<wire::ChatContentPart> = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => {
                Some(wire::ChatContentPart::Text { text: text.clone() })
            }
            ContentBlock::Image { data, mime_type } => {
                let data_url = format!("data:{};base64,{}", mime_type, data);
                Some(wire::ChatContentPart::ImageUrl {
                    image_url: wire::ImageUrl { url: data_url },
                })
            }
            _ => None,
        })
        .collect();

    wire::ChatContent::Parts(parts)
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
