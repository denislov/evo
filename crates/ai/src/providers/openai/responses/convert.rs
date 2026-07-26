use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions};

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::ResponseCreateRequest {
    let tools = ctx.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| wire::ResponseTool {
                tool_type: "function".to_string(),
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect()
    });

    let max_tokens = opts.as_ref().and_then(|o| o.max_tokens);

    let temperature = opts.as_ref().and_then(|o| o.temperature);

    wire::ResponseCreateRequest {
        model: model.id.clone(),
        instructions: ctx.system_prompt.clone(),
        input: ctx.messages.iter().flat_map(convert_message).collect(),
        tools,
        max_output_tokens: max_tokens,
        temperature,
        tool_choice: opts.as_ref().and_then(|o| o.tool_choice.clone()),
        prompt_cache_key: opts.as_ref().and_then(|o| o.session_id.clone()),
        reasoning: opts
            .as_ref()
            .and_then(|options| options.thinking.as_ref())
            .filter(|thinking| thinking.enabled)
            .map(|thinking| wire::ResponseReasoning {
                effort: thinking.effort.clone().unwrap_or_else(|| "medium".into()),
            }),
        stream: true,
    }
}

fn convert_message(msg: &Message) -> Vec<wire::ResponseInputItem> {
    match msg {
        Message::User { content } => vec![wire::ResponseInputItem::Message {
            role: "user".to_string(),
            content: serde_json::json!(
                content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(serde_json::json!({
                            "type": "input_text",
                            "text": text,
                        })),
                        ContentBlock::Image { data, mime_type } => Some(serde_json::json!({
                            "type": "input_image",
                            "image_url": format!("data:{};base64,{}", mime_type, data),
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            ),
        }],
        Message::Assistant { content } => {
            let mut items: Vec<wire::ResponseInputItem> = Vec::new();
            for b in content {
                match b {
                    ContentBlock::Text { text, .. } => {
                        items.push(wire::ResponseInputItem::Message {
                            role: "assistant".to_string(),
                            content: serde_json::json!([{
                                "type": "output_text",
                                "text": text,
                            }]),
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
            items
        }
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => {
            let output = content_to_text(content);
            vec![wire::ResponseInputItem::FunctionCallOutput {
                call_id: tool_call_id.clone(),
                output,
            }]
        }
    }
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
