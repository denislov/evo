use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions};
use crate::providers::common::normalize_tool_call_id;

/// Map evo stop-reason string to our StopReason enum.
pub fn map_stop_reason(s: &str) -> crate::protocol::StopReason {
    match s {
        "end_turn" => crate::protocol::StopReason::Stop,
        "max_tokens" => crate::protocol::StopReason::Length,
        "tool_use" => crate::protocol::StopReason::ToolUse,
        _ => crate::protocol::StopReason::Error,
    }
}

/// Convert a Context to an Anthropic Request.
pub fn build_request(model: &Model, ctx: &Context, opts: &Option<StreamOptions>) -> wire::Request {
    let compat = crate::compatibility::AnthropicMessagesCompat::from_model(model);
    let max_tokens = opts
        .as_ref()
        .and_then(|o| o.max_tokens)
        .or(Some(model.max_tokens))
        .unwrap_or(4096);

    let system = ctx.system_prompt.as_ref().map(|sp| {
        vec![wire::SystemBlock {
            block_type: "text".into(),
            text: sp.clone(),
            cache_control: Some(wire::CacheControl {
                cache_type: "ephemeral".into(),
            }),
        }]
    });

    let mut messages = convert_messages(&ctx.messages);
    // Cache the conversation history by marking the last user message, mirroring
    // the TypeScript reference (`anthropic-messages.ts`). Without this, every
    // turn re-sends the full history as non-cached input, so `input_tokens`
    // (and thus our accumulated `usage.input`) grows with conversation length
    // and history is billed at the full input rate instead of cache_read.
    add_cache_control_to_last_user_message(&mut messages);

    let tools = ctx.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| wire::ToolDef {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
                cache_control: compat
                    .supports_cache_control_on_tools
                    .unwrap_or(false)
                    .then(|| wire::CacheControl {
                        cache_type: "ephemeral".into(),
                    }),
            })
            .collect()
    });

    let temperature = if compat.supports_temperature == Some(false) {
        None
    } else {
        opts.as_ref().and_then(|o| o.temperature)
    };

    let thinking = opts.as_ref().and_then(|o| {
        o.thinking
            .as_ref()
            .filter(|t| t.enabled)
            .map(|t| wire::ThinkingConfig {
                think_type: if compat.force_adaptive_thinking == Some(true) {
                    "adaptive".into()
                } else if t.budget_tokens.is_some() {
                    "enabled".into()
                } else {
                    "auto".into()
                },
                budget_tokens: t.budget_tokens,
            })
    });

    let tool_choice = opts.as_ref().and_then(|o| o.tool_choice.clone());

    wire::Request {
        model: model.id.clone(),
        max_tokens,
        messages,
        system,
        tools,
        temperature,
        thinking,
        tool_choice,
        stream: true,
    }
}

/// Convert evo Messages to Anthropic request messages.
/// Handles consecutive ToolResult coalescing into a single user turn.
fn convert_messages(messages: &[Message]) -> Vec<wire::RequestMessage> {
    let mut result: Vec<wire::RequestMessage> = Vec::new();

    for msg in messages {
        match msg {
            Message::User { content } => {
                result.push(wire::RequestMessage {
                    role: "user".into(),
                    content: convert_content(content),
                });
            }
            Message::Assistant { content } => {
                result.push(wire::RequestMessage {
                    role: "assistant".into(),
                    content: convert_content(content),
                });
            }
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                let tool_content = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": normalize_tool_call_id(tool_call_id, None),
                    "content": convert_content(content),
                });

                // Coalesce: if the last message is also a user-role, append
                // the tool_result to its content array; otherwise push a new user message.
                if let Some(last) = result.last_mut()
                    && last.role == "user"
                    && let Some(arr) = last.content.as_array_mut()
                {
                    arr.push(tool_content);
                    continue;
                }
                result.push(wire::RequestMessage {
                    role: "user".into(),
                    content: serde_json::json!([tool_content]),
                });
            }
        }
    }

    result
}

/// Attach `cache_control: ephemeral` to the final content block of the last
/// user-role message. Anthropic caches the prefix up to and including the
/// breakpoint, so this lets the conversation history be served from the prompt
/// cache on subsequent turns. No-op when there are no messages or the last
/// message is not user-role (e.g. trailing assistant turn).
fn add_cache_control_to_last_user_message(messages: &mut [wire::RequestMessage]) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    if last.role != "user" {
        return;
    }
    let Some(arr) = last.content.as_array_mut() else {
        return;
    };
    let Some(block) = arr.last_mut() else {
        return;
    };
    // User-role blocks are text / image / tool_result, all of which accept
    // cache_control. Don't overwrite an existing breakpoint.
    if block.get("cache_control").is_none() {
        block["cache_control"] = serde_json::json!({ "type": "ephemeral" });
    }
}

/// Convert evo ContentBlocks to Anthropic-compatible JSON array.
fn convert_content(blocks: &[ContentBlock]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text, .. } => {
                serde_json::json!({ "type": "text", "text": text })
            }
            ContentBlock::Thinking { thinking, .. } => {
                serde_json::json!({ "type": "thinking", "thinking": thinking })
            }
            ContentBlock::Image { data, mime_type } => {
                serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": data,
                    }
                })
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                serde_json::json!({
                    "type": "tool_use",
                    "id": normalize_tool_call_id(id, None),
                    "name": name,
                    "input": arguments,
                })
            }
        })
        .collect();
    serde_json::Value::Array(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelCost, ModelInput};

    #[test]
    fn normalize_valid_id_passes_through() {
        assert_eq!(normalize_tool_call_id("toolu_01", None), "toolu_01");
        assert_eq!(normalize_tool_call_id("call-abc-123", None), "call-abc-123");
    }

    #[test]
    fn normalize_invalid_id_sanitized() {
        let result = normalize_tool_call_id("tool*use!001", None);
        assert!(!result.contains('*'));
        assert!(!result.contains('!'));
    }

    #[test]
    fn map_stop_reason_end_turn() {
        assert_eq!(
            map_stop_reason("end_turn"),
            crate::protocol::StopReason::Stop
        );
    }

    #[test]
    fn map_stop_reason_tool_use() {
        assert_eq!(
            map_stop_reason("tool_use"),
            crate::protocol::StopReason::ToolUse
        );
    }

    #[test]
    fn map_stop_reason_max_tokens() {
        assert_eq!(
            map_stop_reason("max_tokens"),
            crate::protocol::StopReason::Length
        );
    }

    #[test]
    fn map_stop_reason_unknown() {
        assert_eq!(
            map_stop_reason("weird_reason"),
            crate::protocol::StopReason::Error
        );
    }

    #[test]
    fn build_basic_request() {
        let model = Model {
            id: "claude-haiku-4-5".into(),
            name: "Haiku".into(),
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 1.0,
                output: 5.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200000,
            max_tokens: 8192,
            headers: None,
            compat: None,
        };
        let ctx = Context {
            system_prompt: Some("Be helpful.".into()),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: "Hello".into(),
                    text_signature: None,
                }],
            }],
            tools: None,
        };
        let req = build_request(&model, &ctx, &None);
        assert_eq!(req.model, "claude-haiku-4-5");
        assert_eq!(req.max_tokens, 8192);
        assert!(req.stream);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert!(req.system.is_some());
    }

    #[test]
    fn tool_result_coalescing() {
        let messages = vec![
            Message::ToolResult {
                tool_call_id: "call_1".into(),
                tool_name: None,
                is_error: None,
                content: vec![ContentBlock::Text {
                    text: "result1".into(),
                    text_signature: None,
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_2".into(),
                tool_name: None,
                is_error: None,
                content: vec![ContentBlock::Text {
                    text: "result2".into(),
                    text_signature: None,
                }],
            },
        ];
        let converted = convert_messages(&messages);
        assert_eq!(converted.len(), 1); // coalesced into one user message
        assert_eq!(converted[0].role, "user");
        let content = converted[0].content.as_array().unwrap();
        assert_eq!(content.len(), 2);
    }

    fn cache_test_model() -> Model {
        Model {
            id: "claude-haiku-4-5".into(),
            name: "Haiku".into(),
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 1.0,
                output: 5.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200000,
            max_tokens: 8192,
            headers: None,
            compat: None,
        }
    }

    fn user_text(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.into(),
                text_signature: None,
            }],
        }
    }

    fn assistant_text(text: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::Text {
                text: text.into(),
                text_signature: None,
            }],
        }
    }

    #[test]
    fn last_user_message_gets_cache_control() {
        // History caching: the final user message's last content block must
        // carry `cache_control: ephemeral` so the conversation prefix is served
        // from the prompt cache on subsequent turns.
        let ctx = Context {
            system_prompt: Some("Be helpful.".into()),
            messages: vec![
                user_text("first question"),
                assistant_text("first answer"),
                user_text("second question"),
            ],
            tools: None,
        };
        let req = build_request(&cache_test_model(), &ctx, &None);
        assert_eq!(req.messages.len(), 3);

        // Only the last user message should be marked.
        let first_user = req.messages[0].content.as_array().unwrap();
        assert!(first_user[0].get("cache_control").is_none());

        let assistant = req.messages[1].content.as_array().unwrap();
        assert!(assistant[0].get("cache_control").is_none());

        let last_user = req.messages[2].content.as_array().unwrap();
        assert_eq!(last_user[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn no_cache_control_when_trailing_message_is_assistant() {
        // If the conversation ends on an assistant turn, there is no user-role
        // message to mark; caching must be a no-op rather than crashing.
        let ctx = Context {
            system_prompt: None,
            messages: vec![user_text("hi"), assistant_text("hello")],
            tools: None,
        };
        let req = build_request(&cache_test_model(), &ctx, &None);
        for msg in &req.messages {
            for block in msg.content.as_array().unwrap() {
                assert!(block.get("cache_control").is_none());
            }
        }
    }

    #[test]
    fn coalesced_tool_result_gets_cache_control() {
        // Tool results are coalesced into a user-role message; the final
        // tool_result block must receive the cache breakpoint.
        let ctx = Context {
            system_prompt: None,
            messages: vec![
                assistant_text("calling tools"),
                Message::ToolResult {
                    tool_call_id: "call_1".into(),
                    tool_name: None,
                    is_error: None,
                    content: vec![ContentBlock::Text {
                        text: "r1".into(),
                        text_signature: None,
                    }],
                },
                Message::ToolResult {
                    tool_call_id: "call_2".into(),
                    tool_name: None,
                    is_error: None,
                    content: vec![ContentBlock::Text {
                        text: "r2".into(),
                        text_signature: None,
                    }],
                },
            ],
            tools: None,
        };
        let req = build_request(&cache_test_model(), &ctx, &None);
        assert_eq!(req.messages.len(), 2);
        let user_msg = req.messages.last().unwrap();
        assert_eq!(user_msg.role, "user");
        let blocks = user_msg.content.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        // First tool_result: not marked.
        assert!(blocks[0].get("cache_control").is_none());
        // Last tool_result: marked.
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
    }
}
