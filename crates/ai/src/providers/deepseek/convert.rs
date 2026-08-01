use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions};

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: Option<&StreamOptions>,
) -> Result<wire::ResponseCreateRequest, String> {
    let reasoning = resolve_reasoning(model, opts)?;
    let thinking_enabled = model.reasoning
        && reasoning
            .as_ref()
            .is_none_or(|reasoning| reasoning.effort != "none");
    let temperature = (!thinking_enabled)
        .then(|| opts.and_then(|options| options.temperature))
        .flatten();
    if temperature
        .is_some_and(|temperature| !temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
    {
        return Err("DeepSeek temperature must be between 0.0 and 2.0".into());
    }

    let tools = ctx.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| wire::ResponseTool {
                tool_type: "function".into(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            })
            .collect()
    });

    let input = ctx
        .messages
        .iter()
        .map(convert_message)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(wire::ResponseCreateRequest {
        model: model.id.clone(),
        instructions: ctx.system_prompt.clone(),
        input,
        tools,
        max_output_tokens: opts.and_then(|options| options.max_tokens),
        temperature,
        tool_choice: opts.and_then(|options| options.tool_choice.clone()),
        reasoning,
        stream: true,
    })
}

fn resolve_reasoning(
    model: &Model,
    opts: Option<&StreamOptions>,
) -> Result<Option<wire::ResponseReasoning>, String> {
    if !model.reasoning {
        return Ok(None);
    }
    let Some(thinking) = opts.and_then(|options| options.thinking.as_ref()) else {
        return Ok(None);
    };
    if !thinking.enabled {
        return Ok(Some(wire::ResponseReasoning {
            effort: "none".into(),
        }));
    }

    let requested = thinking.effort.as_deref().unwrap_or("high");
    let effort = model
        .thinking_level_map
        .as_ref()
        .and_then(|mapping| mapping.resolve(requested))
        .unwrap_or_else(|| requested.to_owned());
    if !matches!(effort.as_str(), "low" | "high" | "max") {
        return Err(format!(
            "DeepSeek reasoning effort `{effort}` is unsupported; expected low, high, or max"
        ));
    }
    Ok(Some(wire::ResponseReasoning { effort }))
}

fn convert_message(message: &Message) -> Result<Vec<wire::ResponseInputItem>, String> {
    match message {
        Message::User { content } => {
            let mut parts = Vec::new();
            for block in content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        parts.push(wire::ResponseMessageContent {
                            content_type: "input_text".into(),
                            text: text.clone(),
                        });
                    }
                    ContentBlock::Image { .. } => {
                        return Err("DeepSeek Responses does not support image input".to_string());
                    }
                    _ => {}
                }
            }
            Ok(vec![wire::ResponseInputItem::Message {
                role: "user".into(),
                content: parts,
            }])
        }
        Message::Assistant { content } => {
            let mut items = Vec::new();
            let has_tool_call = content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall { .. }));
            for block in content {
                match block {
                    ContentBlock::Thinking {
                        thinking,
                        thinking_signature,
                        ..
                    } if !thinking.is_empty() => {
                        let Some(id) = thinking_signature.clone() else {
                            // Responses reasoning items require their provider item id. Older
                            // Chat Completions turns do not carry one and cannot be replayed as a
                            // valid DeepSeek Responses reasoning item.
                            if has_tool_call {
                                return Err(
                                    "DeepSeek tool-call reasoning is missing its Responses item id"
                                        .into(),
                                );
                            }
                            continue;
                        };
                        items.push(wire::ResponseInputItem::Reasoning {
                            id,
                            summary: Vec::new(),
                            content: vec![wire::ResponseReasoningContent {
                                content_type: "reasoning_text".into(),
                                text: thinking.clone(),
                            }],
                        });
                    }
                    ContentBlock::Text { text, .. } => {
                        items.push(wire::ResponseInputItem::Message {
                            role: "assistant".into(),
                            content: vec![wire::ResponseMessageContent {
                                content_type: "output_text".into(),
                                text: text.clone(),
                            }],
                        });
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } => {
                        items.push(wire::ResponseInputItem::FunctionCall {
                            call_id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            Ok(items)
        }
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => Ok(vec![wire::ResponseInputItem::FunctionCallOutput {
            call_id: tool_call_id.clone(),
            output: content_to_text(content),
        }]),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ThinkingConfig, Tool};

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.into(),
            text_signature: None,
        }
    }

    #[test]
    fn reasoning_and_tools_round_trip_in_order() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: Some("be helpful".into()),
            messages: vec![
                Message::Assistant {
                    content: vec![
                        ContentBlock::Thinking {
                            thinking: "need the weather".into(),
                            thinking_signature: Some("rs_1".into()),
                            redacted: None,
                        },
                        ContentBlock::ToolCall {
                            id: "call_1".into(),
                            name: "weather".into(),
                            arguments: serde_json::json!({"city": "杭州"}),
                            thought_signature: None,
                        },
                    ],
                },
                Message::ToolResult {
                    tool_call_id: "call_1".into(),
                    tool_name: Some("weather".into()),
                    is_error: Some(false),
                    content: vec![text("晴")],
                },
            ],
            tools: Some(vec![Tool {
                name: "weather".into(),
                description: Some("Get weather".into()),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        };
        let options = StreamOptions {
            temperature: Some(0.7),
            thinking: Some(ThinkingConfig {
                enabled: true,
                budget_tokens: None,
                effort: Some("xhigh".into()),
            }),
            ..Default::default()
        };

        let request = build_request(&model, &context, Some(&options)).expect("request converts");
        let value = serde_json::to_value(request).expect("request serializes");
        assert_eq!(value["reasoning"]["effort"], "max");
        assert!(value.get("temperature").is_none());
        assert!(value.get("prompt_cache_key").is_none());
        assert_eq!(value["input"][0]["type"], "reasoning");
        assert_eq!(value["input"][0]["id"], "rs_1");
        assert_eq!(value["input"][0]["content"][0]["text"], "need the weather");
        assert_eq!(value["input"][1]["type"], "function_call");
        assert_eq!(value["input"][2]["type"], "function_call_output");
    }

    #[test]
    fn disabled_reasoning_uses_none_and_allows_temperature() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![text("hello")],
            }],
            tools: None,
        };
        let options = StreamOptions {
            temperature: Some(0.5),
            thinking: Some(ThinkingConfig {
                enabled: false,
                budget_tokens: None,
                effort: None,
            }),
            ..Default::default()
        };

        let request = build_request(&model, &context, Some(&options)).expect("request converts");
        let value = serde_json::to_value(request).expect("request serializes");
        assert_eq!(value["reasoning"]["effort"], "none");
        assert_eq!(value["temperature"], 0.5);
    }

    #[test]
    fn image_input_is_rejected_before_transport() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![ContentBlock::Image {
                    data: "AA==".into(),
                    mime_type: "image/png".into(),
                }],
            }],
            tools: None,
        };

        let error = build_request(&model, &context, None).expect_err("images are unsupported");
        assert!(error.contains("does not support image"));
    }

    #[test]
    fn tool_call_reasoning_without_item_id_is_rejected() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::Assistant {
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "legacy reasoning".into(),
                        thinking_signature: None,
                        redacted: None,
                    },
                    ContentBlock::ToolCall {
                        id: "call_legacy".into(),
                        name: "weather".into(),
                        arguments: serde_json::json!({}),
                        thought_signature: None,
                    },
                ],
            }],
            tools: None,
        };

        let error = build_request(&model, &context, None)
            .expect_err("reasoning item id is required for tool-call replay");
        assert!(error.contains("missing its Responses item id"));
    }
}
