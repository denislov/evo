use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions, ToolCallKind, ToolKind};
use std::collections::HashMap;

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
    let responses = opts.and_then(|options| options.responses.as_ref());
    if opts
        .and_then(|options| options.session_id.as_ref())
        .is_some()
    {
        return Err("DeepSeek Responses is stateless and does not support session_id".into());
    }
    if responses
        .and_then(|options| options.prompt_cache_key.as_ref())
        .is_some()
    {
        return Err(
            "DeepSeek Responses does not support prompt_cache_key; caching is automatic".into(),
        );
    }
    if temperature
        .is_some_and(|temperature| !temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
    {
        return Err("DeepSeek temperature must be between 0.0 and 2.0".into());
    }
    let top_p = (!thinking_enabled)
        .then(|| responses.and_then(|options| options.top_p))
        .flatten();
    if top_p.is_some_and(|top_p| !top_p.is_finite() || !(0.0..=1.0).contains(&top_p)) {
        return Err("DeepSeek top_p must be between 0.0 and 1.0".into());
    }
    let top_logprobs = responses.and_then(|options| options.top_logprobs);
    if top_logprobs.is_some_and(|value| value > 20) {
        return Err("DeepSeek top_logprobs must be between 0 and 20".into());
    }
    let user = responses.and_then(|options| options.user.clone());
    if user.as_ref().is_some_and(|value| value.trim().is_empty()) {
        return Err("DeepSeek user must not be empty".into());
    }
    let tool_choice = opts.and_then(|options| options.tool_choice.clone());
    if thinking_enabled
        && tool_choice
            .as_ref()
            .is_some_and(|choice| !matches!(choice.as_str(), Some("auto" | "none")))
    {
        return Err(
            "DeepSeek thinking mode only supports tool_choice `auto`, `none`, or omission".into(),
        );
    }

    let tools = ctx
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| match tool.kind {
                    ToolKind::Function => Ok(wire::ResponseTool::Function {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    }),
                    ToolKind::WebSearch => Ok(wire::ResponseTool::WebSearch),
                    ToolKind::Custom if tool.name == "apply_patch" => {
                        Ok(wire::ResponseTool::Custom {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                        })
                    }
                    ToolKind::Custom => Err(format!(
                        "DeepSeek only supports the custom tool `apply_patch`, not `{}`",
                        tool.name
                    )),
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;

    let tool_call_kinds = ctx
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolCall { id, kind, .. } => Some((id.as_str(), *kind)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    let input = ctx
        .messages
        .iter()
        .map(|message| convert_message(message, &tool_call_kinds))
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
        top_p,
        top_logprobs,
        tool_choice,
        reasoning,
        text: responses
            .and_then(|options| options.text_format.clone())
            .map(|format| wire::ResponseText { format }),
        user,
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

fn convert_message(
    message: &Message,
    tool_call_kinds: &HashMap<&str, ToolCallKind>,
) -> Result<Vec<wire::ResponseInputItem>, String> {
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
            Ok(vec![known(wire::ResponseKnownInputItem::Message {
                role: "user".into(),
                content: parts,
            })])
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
                        provider_metadata,
                        ..
                    } if !thinking.is_empty() => {
                        let item_id = provider_metadata
                            .as_ref()
                            .filter(|metadata| metadata.api == super::API_NAME)
                            .and_then(|metadata| metadata.item_id.clone())
                            // Compatibility for sessions written by the initial
                            // DeepSeek provider before structured metadata.
                            .or_else(|| {
                                thinking_signature
                                    .as_ref()
                                    .filter(|value| is_legacy_deepseek_item_id(value))
                                    .cloned()
                            });
                        let Some(id) = item_id else {
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
                        items.push(known(wire::ResponseKnownInputItem::Reasoning {
                            id,
                            summary: Vec::new(),
                            content: vec![wire::ResponseReasoningContent {
                                content_type: "reasoning_text".into(),
                                text: thinking.clone(),
                            }],
                        }));
                    }
                    ContentBlock::Text { text, .. } => {
                        items.push(known(wire::ResponseKnownInputItem::Message {
                            role: "assistant".into(),
                            content: vec![wire::ResponseMessageContent {
                                content_type: "output_text".into(),
                                text: text.clone(),
                            }],
                        }));
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        kind,
                        ..
                    } => {
                        items.push(known(match kind {
                            ToolCallKind::Function => wire::ResponseKnownInputItem::FunctionCall {
                                call_id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.to_string(),
                            },
                            ToolCallKind::Custom => wire::ResponseKnownInputItem::CustomToolCall {
                                call_id: id.clone(),
                                name: name.clone(),
                                input: arguments
                                    .as_str()
                                    .ok_or_else(|| {
                                        "DeepSeek custom tool input must be a string".to_string()
                                    })?
                                    .to_owned(),
                            },
                        }));
                    }
                    ContentBlock::ProviderItem { api, item } => {
                        if api != super::API_NAME {
                            return Err(format!(
                                "cannot replay provider item from `{api}` through `{}`",
                                super::API_NAME
                            ));
                        }
                        if item.get("type").and_then(serde_json::Value::as_str)
                            != Some("web_search_call")
                        {
                            return Err("DeepSeek provider item must be a web_search_call".into());
                        }
                        items.push(wire::ResponseInputItem::Provider(item.clone()));
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
        } => Ok(vec![known(
            match tool_call_kinds
                .get(tool_call_id.as_str())
                .copied()
                .unwrap_or_default()
            {
                ToolCallKind::Function => wire::ResponseKnownInputItem::FunctionCallOutput {
                    call_id: tool_call_id.clone(),
                    output: content_to_text(content),
                },
                ToolCallKind::Custom => wire::ResponseKnownInputItem::CustomToolCallOutput {
                    call_id: tool_call_id.clone(),
                    output: content_to_text(content),
                },
            },
        )]),
    }
}

fn known(item: wire::ResponseKnownInputItem) -> wire::ResponseInputItem {
    wire::ResponseInputItem::Known(item)
}

fn is_legacy_deepseek_item_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
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
    use crate::protocol::{ResponsesOptions, ResponsesTextFormat, ThinkingConfig, Tool};

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
                            thinking_signature: None,
                            provider_metadata: Some(crate::protocol::ProviderMetadata {
                                api: "deepseek-responses".into(),
                                item_id: Some("rs_1".into()),
                                encrypted_content: None,
                            }),
                            redacted: None,
                        },
                        ContentBlock::ToolCall {
                            id: "call_1".into(),
                            name: "weather".into(),
                            arguments: serde_json::json!({"city": "杭州"}),
                            kind: ToolCallKind::Function,
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
            tools: Some(vec![Tool::function(
                "weather",
                Some("Get weather".into()),
                serde_json::json!({"type": "object"}),
            )]),
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
    fn supported_responses_options_and_tool_types_serialize() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![text("return JSON")],
            }],
            tools: Some(vec![
                Tool::web_search(),
                Tool::custom("apply_patch", Some("Apply a patch".into())),
            ]),
        };
        let options = StreamOptions {
            temperature: Some(0.4),
            thinking: Some(ThinkingConfig {
                enabled: false,
                budget_tokens: None,
                effort: None,
            }),
            responses: Some(ResponsesOptions {
                top_p: Some(0.8),
                top_logprobs: Some(4),
                text_format: Some(ResponsesTextFormat::JsonSchema {
                    name: "answer".into(),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": {"answer": {"type": "string"}},
                        "required": ["answer"],
                        "additionalProperties": false
                    }),
                    strict: Some(true),
                    description: None,
                }),
                user: Some("tenant-1".into()),
                prompt_cache_key: None,
            }),
            ..StreamOptions::default()
        };

        let value = serde_json::to_value(
            build_request(&model, &context, Some(&options)).expect("request converts"),
        )
        .expect("request serializes");
        assert_eq!(value["temperature"], 0.4);
        assert_eq!(value["top_p"], 0.8);
        assert_eq!(value["top_logprobs"], 4);
        assert_eq!(value["user"], "tenant-1");
        assert_eq!(value["text"]["format"]["type"], "json_schema");
        assert_eq!(value["tools"][0]["type"], "web_search");
        assert_eq!(value["tools"][1]["type"], "custom");
        assert_eq!(value["tools"][1]["name"], "apply_patch");
    }

    #[test]
    fn thinking_rejects_required_tool_choice_and_omits_sampling() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![text("use a tool")],
            }],
            tools: None,
        };
        let mut options = StreamOptions {
            temperature: Some(0.7),
            tool_choice: Some(serde_json::json!("auto")),
            thinking: Some(ThinkingConfig {
                enabled: true,
                budget_tokens: None,
                effort: Some("low".into()),
            }),
            responses: Some(ResponsesOptions {
                top_p: Some(0.6),
                ..ResponsesOptions::default()
            }),
            ..StreamOptions::default()
        };
        let value = serde_json::to_value(
            build_request(&model, &context, Some(&options)).expect("auto is supported"),
        )
        .expect("request serializes");
        assert!(value.get("temperature").is_none());
        assert!(value.get("top_p").is_none());

        options.tool_choice = Some(serde_json::json!("required"));
        let error = build_request(&model, &context, Some(&options))
            .expect_err("required is rejected locally in thinking mode");
        assert!(error.contains("only supports tool_choice"));
    }

    #[test]
    fn custom_tool_and_web_search_items_round_trip() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::Assistant {
                    content: vec![
                        ContentBlock::ToolCall {
                            id: "call_custom".into(),
                            name: "apply_patch".into(),
                            arguments: serde_json::Value::String("*** Begin Patch\n".into()),
                            kind: ToolCallKind::Custom,
                            thought_signature: None,
                        },
                        ContentBlock::ProviderItem {
                            api: "deepseek-responses".into(),
                            item: serde_json::json!({
                                "type": "web_search_call",
                                "id": "web_1",
                                "status": "completed",
                                "action": {"type": "search", "queries": ["DeepSeek"]}
                            }),
                        },
                    ],
                },
                Message::ToolResult {
                    tool_call_id: "call_custom".into(),
                    tool_name: Some("apply_patch".into()),
                    is_error: Some(false),
                    content: vec![text("Done!")],
                },
            ],
            tools: Some(vec![Tool::custom("apply_patch", None), Tool::web_search()]),
        };

        let value =
            serde_json::to_value(build_request(&model, &context, None).expect("request converts"))
                .expect("request serializes");
        assert_eq!(value["input"][0]["type"], "custom_tool_call");
        assert_eq!(value["input"][0]["input"], "*** Begin Patch\n");
        assert_eq!(value["input"][1]["type"], "web_search_call");
        assert_eq!(value["input"][2]["type"], "custom_tool_call_output");
    }

    #[test]
    fn stateless_and_automatic_cache_options_are_rejected() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![text("hello")],
            }],
            tools: None,
        };
        let session_options = StreamOptions {
            session_id: Some("session-1".into()),
            ..StreamOptions::default()
        };
        assert!(
            build_request(&model, &context, Some(&session_options))
                .expect_err("session id is unsupported")
                .contains("stateless")
        );
        let cache_options = StreamOptions {
            responses: Some(ResponsesOptions {
                prompt_cache_key: Some("cache-1".into()),
                ..ResponsesOptions::default()
            }),
            ..StreamOptions::default()
        };
        assert!(
            build_request(&model, &context, Some(&cache_options))
                .expect_err("prompt cache key is unsupported")
                .contains("caching is automatic")
        );
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
                        provider_metadata: None,
                        redacted: None,
                    },
                    ContentBlock::ToolCall {
                        id: "call_legacy".into(),
                        name: "weather".into(),
                        arguments: serde_json::json!({}),
                        kind: ToolCallKind::Function,
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

    #[test]
    fn initial_provider_uuid_signature_migrates_to_reasoning_item_id() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let legacy_id = "123e4567-e89b-12d3-a456-426614174000";
        let context = Context {
            system_prompt: None,
            messages: vec![Message::Assistant {
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "legacy DeepSeek reasoning".into(),
                        thinking_signature: Some(legacy_id.into()),
                        provider_metadata: None,
                        redacted: None,
                    },
                    ContentBlock::ToolCall {
                        id: "call_legacy".into(),
                        name: "weather".into(),
                        arguments: serde_json::json!({}),
                        kind: ToolCallKind::Function,
                        thought_signature: None,
                    },
                ],
            }],
            tools: None,
        };

        let value = serde_json::to_value(
            build_request(&model, &context, None).expect("initial provider session migrates"),
        )
        .expect("request serializes");
        assert_eq!(value["input"][0]["type"], "reasoning");
        assert_eq!(value["input"][0]["id"], legacy_id);
    }
}
