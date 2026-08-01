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
        o.thinking.as_ref().filter(|t| t.enabled).map(|t| {
            let adaptive = compat.force_adaptive_thinking == Some(true);
            wire::ThinkingConfig {
                think_type: if adaptive {
                    "adaptive".into()
                } else if t.budget_tokens.is_some() {
                    "enabled".into()
                } else {
                    "auto".into()
                },
                // `adaptive` rejects an explicit budget on the wire.
                budget_tokens: if adaptive { None } else { t.budget_tokens },
            }
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
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => {
                Some(serde_json::json!({ "type": "text", "text": text }))
            }
            ContentBlock::Thinking { thinking, .. } => {
                Some(serde_json::json!({ "type": "thinking", "thinking": thinking }))
            }
            ContentBlock::Image { data, mime_type } => Some(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime_type,
                    "data": data,
                }
            })),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(serde_json::json!({
                "type": "tool_use",
                "id": normalize_tool_call_id(id, None),
                "name": name,
                "input": arguments,
            })),
            ContentBlock::ProviderItem { .. } => None,
        })
        .collect();
    serde_json::Value::Array(items)
}
