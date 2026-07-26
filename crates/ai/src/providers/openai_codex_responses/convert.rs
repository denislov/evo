use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions};

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::RequestBody {
    wire::RequestBody {
        model: model.id.clone(),
        store: Some(false),
        stream: Some(true),
        instructions: Some(
            ctx.system_prompt
                .clone()
                .unwrap_or_else(|| "You are a helpful assistant.".into()),
        ),
        input: ctx.messages.iter().flat_map(convert_message).collect(),
        tools: ctx.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| wire::CodexTool {
                    tool_type: "function".into(),
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                    strict: None,
                })
                .collect()
        }),
        tool_choice: opts
            .as_ref()
            .and_then(|options| options.tool_choice.as_ref())
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| Some("auto".into())),
        parallel_tool_calls: Some(true),
        temperature: opts.as_ref().and_then(|o| o.temperature),
        max_output_tokens: opts.as_ref().and_then(|options| options.max_tokens),
        reasoning: opts
            .as_ref()
            .and_then(|o| o.thinking.as_ref())
            .filter(|t| t.enabled)
            .map(|thinking| wire::CodexReasoning {
                effort: thinking.effort.clone().unwrap_or_else(|| "medium".into()),
                summary: "auto".into(),
            }),
        service_tier: None,
        text: Some(wire::CodexText {
            verbosity: "low".into(),
        }),
        include: vec!["reasoning.encrypted_content".into()],
        prompt_cache_key: opts.as_ref().and_then(|o| o.session_id.clone()),
    }
}

fn convert_message(message: &Message) -> Vec<serde_json::Value> {
    match message {
        Message::User { content } => vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": content.iter().filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(serde_json::json!({
                    "type": "input_text",
                    "text": text,
                })),
                ContentBlock::Image { data, mime_type } => Some(serde_json::json!({
                    "type": "input_image",
                    "image_url": format!("data:{};base64,{}", mime_type, data),
                })),
                _ => None,
            }).collect::<Vec<_>>(),
        })],
        Message::Assistant { content } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                    }],
                })),
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some(serde_json::json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments.to_string(),
                })),
                _ => None,
            })
            .collect(),
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => vec![serde_json::json!({
            "type": "function_call_output",
            "call_id": tool_call_id,
            "output": content.iter().filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n"),
        })],
    }
}
