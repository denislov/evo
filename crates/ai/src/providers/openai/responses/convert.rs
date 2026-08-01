use super::wire;
use crate::model::Model;
use crate::protocol::{ContentBlock, Context, Message, StreamOptions, ToolCallKind, ToolKind};
use std::collections::HashMap;

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::ResponseCreateRequest {
    let tools = ctx.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| match tool.kind {
                ToolKind::Function => wire::ResponseTool::Function {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
                ToolKind::WebSearch => wire::ResponseTool::WebSearch,
                ToolKind::Custom => wire::ResponseTool::Custom {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                },
            })
            .collect()
    });

    let max_tokens = opts.as_ref().and_then(|o| o.max_tokens);

    let temperature = opts.as_ref().and_then(|o| o.temperature);

    let responses = opts.as_ref().and_then(|options| options.responses.as_ref());
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

    wire::ResponseCreateRequest {
        model: model.id.clone(),
        instructions: ctx.system_prompt.clone(),
        input: ctx
            .messages
            .iter()
            .flat_map(|message| convert_message(message, &tool_call_kinds))
            .collect(),
        tools,
        max_output_tokens: max_tokens,
        temperature,
        top_p: responses.and_then(|options| options.top_p),
        top_logprobs: responses.and_then(|options| options.top_logprobs),
        tool_choice: opts.as_ref().and_then(|o| o.tool_choice.clone()),
        prompt_cache_key: responses.and_then(|options| options.prompt_cache_key.clone()),
        reasoning: opts
            .as_ref()
            .and_then(|options| options.thinking.as_ref())
            .filter(|thinking| thinking.enabled)
            .map(|thinking| wire::ResponseReasoning {
                effort: thinking.effort.clone().unwrap_or_else(|| "medium".into()),
            }),
        text: responses
            .and_then(|options| options.text_format.clone())
            .map(|format| wire::ResponseText { format }),
        user: responses.and_then(|options| options.user.clone()),
        stream: true,
    }
}

fn convert_message(
    msg: &Message,
    tool_call_kinds: &HashMap<&str, ToolCallKind>,
) -> Vec<wire::ResponseInputItem> {
    match msg {
        Message::User { content } => vec![known(wire::ResponseKnownInputItem::Message {
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
        })],
        Message::Assistant { content } => {
            let mut items: Vec<wire::ResponseInputItem> = Vec::new();
            for b in content {
                match b {
                    ContentBlock::Thinking {
                        thinking,
                        provider_metadata: Some(metadata),
                        ..
                    } if metadata.api == "openai-responses" => {
                        let Some(id) = metadata.item_id.clone() else {
                            continue;
                        };
                        items.push(known(wire::ResponseKnownInputItem::Reasoning {
                            id,
                            summary: Vec::new(),
                            content: (!thinking.is_empty()).then(|| {
                                serde_json::json!([{
                                    "type": "reasoning_text",
                                    "text": thinking,
                                }])
                            }),
                            encrypted_content: metadata.encrypted_content.clone(),
                        }));
                    }
                    ContentBlock::Text { text, .. } => {
                        items.push(known(wire::ResponseKnownInputItem::Message {
                            role: "assistant".to_string(),
                            content: serde_json::json!([{
                                "type": "output_text",
                                "text": text,
                            }]),
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
                                input: arguments.as_str().unwrap_or_default().to_owned(),
                            },
                        }));
                    }
                    ContentBlock::ProviderItem { api, item } if api == "openai-responses" => {
                        items.push(wire::ResponseInputItem::Provider(item.clone()));
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
            vec![known(
                match tool_call_kinds
                    .get(tool_call_id.as_str())
                    .copied()
                    .unwrap_or_default()
                {
                    ToolCallKind::Function => wire::ResponseKnownInputItem::FunctionCallOutput {
                        call_id: tool_call_id.clone(),
                        output,
                    },
                    ToolCallKind::Custom => wire::ResponseKnownInputItem::CustomToolCallOutput {
                        call_id: tool_call_id.clone(),
                        output,
                    },
                },
            )]
        }
    }
}

fn known(item: wire::ResponseKnownInputItem) -> wire::ResponseInputItem {
    wire::ResponseInputItem::Known(item)
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
