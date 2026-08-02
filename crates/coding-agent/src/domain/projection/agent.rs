use agent_core::api::agent::AgentEvent;
use ai::api::conversation::ContentBlock;
use ai::api::stream::AssistantMessageEvent;

use crate::events::agent::AgentStreamEvent;
use crate::events::delegation::{DelegationEvent, DelegationEventContext};
use crate::events::message::MessageEvent;
use crate::events::prompt_stream::PromptStreamEvent;
use crate::events::runtime::RuntimeEvent;
use crate::events::tool::ToolEvent;
use crate::kernel::ids::ProfileId;
use crate::profiles::ProfileKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEventMappingContext {
    operation_id: String,
    turn_id: String,
    assistant_message_id: Option<String>,
    reasoning_duration_millis: Option<u64>,
}

impl AgentEventMappingContext {
    pub(crate) fn new(operation_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
            assistant_message_id: None,
            reasoning_duration_millis: None,
        }
    }

    pub(crate) fn with_assistant_message_id(mut self, message_id: impl Into<String>) -> Self {
        self.assistant_message_id = Some(message_id.into());
        self
    }

    pub(crate) fn with_reasoning_duration_millis(mut self, duration_millis: Option<u64>) -> Self {
        self.reasoning_duration_millis = duration_millis;
        self
    }
}

pub(crate) fn map_agent_event(
    context: &AgentEventMappingContext,
    event: &AgentEvent,
) -> Vec<PromptStreamEvent> {
    match event {
        AgentEvent::TurnStart { turn } => {
            vec![PromptStreamEvent::Agent(AgentStreamEvent::TurnStarted {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                agent_turn: *turn,
            })]
        }
        AgentEvent::BeforeProviderRequest { request } => {
            vec![PromptStreamEvent::Agent(
                AgentStreamEvent::ProviderRequestStarted {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    provider: request.model.provider.clone(),
                    model: request.model.id.clone(),
                    context_window: (request.model.context_window > 0)
                        .then_some(request.model.context_window),
                },
            )]
        }
        AgentEvent::LlmEvent(event) => map_assistant_event(context, event),
        AgentEvent::ToolCallStart {
            tool_call_id,
            tool_name,
            arguments,
        } => vec![PromptStreamEvent::Tool(ToolEvent::Started {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            arguments_json: arguments.to_string(),
        })],
        AgentEvent::ToolCallUpdate {
            tool_call_id,
            tool_name,
            update,
        } => vec![PromptStreamEvent::Tool(ToolEvent::Updated {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            message: content_blocks_text(&update.content),
        })],
        AgentEvent::ToolCallEnd {
            tool_call_id,
            tool_name,
            result,
        } if result.is_error => vec![PromptStreamEvent::Tool(ToolEvent::Failed {
            operation_id: context.operation_id.clone(),
            turn_id: context.turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            message: content_blocks_text(&result.content),
        })],
        AgentEvent::ToolCallEnd {
            tool_call_id,
            tool_name,
            result,
        } => {
            let summary = content_blocks_text(&result.content);
            let mut events = vec![PromptStreamEvent::Tool(ToolEvent::Completed {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                name: tool_name.clone(),
                summary: summary.clone(),
            })];
            if let Some(event) =
                map_delegation_tool_event(context, tool_call_id, tool_name, &summary)
            {
                events.push(event);
            }
            events
        }
        AgentEvent::AgentDone { .. } => Vec::new(),
        AgentEvent::AgentError { .. } => Vec::new(),
        AgentEvent::SessionCompacted {
            summary,
            first_kept_message_id,
            tokens_before,
            details: _,
        } => vec![PromptStreamEvent::Runtime(
            RuntimeEvent::CompactionCompleted {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                summary: summary.clone(),
                first_kept_message_id: first_kept_message_id.clone(),
                tokens_before: *tokens_before,
            },
        )],
    }
}

fn map_assistant_event(
    context: &AgentEventMappingContext,
    event: &AssistantMessageEvent,
) -> Vec<PromptStreamEvent> {
    match event {
        AssistantMessageEvent::Start { .. }
        | AssistantMessageEvent::TextStart { .. }
        | AssistantMessageEvent::ThinkingStart { .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Started {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
            })]
        }
        AssistantMessageEvent::TextDelta { delta, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Delta {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                text: delta.clone(),
            })]
        }
        AssistantMessageEvent::ThinkingDelta { delta, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::ThinkingDelta {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                text: delta.clone(),
            })]
        }
        AssistantMessageEvent::ProviderItemStart {
            content_index,
            partial,
        } => provider_item(partial, *content_index)
            .map(|(id, name, item)| {
                vec![PromptStreamEvent::Tool(ToolEvent::Started {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: id,
                    name,
                    arguments_json: item.to_string(),
                })]
            })
            .unwrap_or_default(),
        AssistantMessageEvent::ProviderItemDelta {
            content_index,
            delta,
            partial,
        } => provider_item(partial, *content_index)
            .map(|(id, name, _)| {
                vec![PromptStreamEvent::Tool(ToolEvent::Updated {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: id,
                    name,
                    message: delta.clone(),
                })]
            })
            .unwrap_or_default(),
        AssistantMessageEvent::ProviderItemEnd {
            content_index,
            partial,
        } => provider_item(partial, *content_index)
            .map(|(id, name, item)| {
                vec![PromptStreamEvent::Tool(ToolEvent::Completed {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: id,
                    name,
                    summary: web_search_completed_summary(item),
                })]
            })
            .unwrap_or_default(),
        AssistantMessageEvent::Error { .. } => Vec::new(),
        AssistantMessageEvent::Done { message, .. } => {
            vec![PromptStreamEvent::Message(MessageEvent::Completed {
                operation_id: context.operation_id.clone(),
                turn_id: context.turn_id.clone(),
                message_id: context.assistant_message_id.clone(),
                final_text: assistant_text(&message.content),
                images: assistant_images(&message.content),
                usage: message.usage.clone(),
                reasoning_duration_millis: context.reasoning_duration_millis,
            })]
        }
        AssistantMessageEvent::TextEnd { .. }
        | AssistantMessageEvent::ThinkingEnd { .. }
        | AssistantMessageEvent::ToolcallStart { .. }
        | AssistantMessageEvent::ToolcallDelta { .. }
        | AssistantMessageEvent::ToolcallEnd { .. } => Vec::new(),
    }
}

pub(crate) fn provider_item(
    partial: &ai::api::conversation::AssistantMessage,
    content_index: u32,
) -> Option<(String, String, &serde_json::Value)> {
    let ContentBlock::ProviderItem { item, .. } = partial.content.get(content_index as usize)?
    else {
        return None;
    };
    let id = item.get("id")?.as_str()?.to_owned();
    let name = item
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider_tool")
        .trim_end_matches("_call")
        .to_owned();
    Some((id, name, item))
}

/// Serializes the terminal state of a provider-executed web-search item so
/// downstream consumers (live projections and the durable session log) can
/// render the action (search queries or opened page) without re-parsing the
/// raw wire item. Falls back to the bare status string when the item carries
/// no action (legacy or incomplete items).
pub(crate) fn web_search_completed_summary(item: &serde_json::Value) -> String {
    let status = item
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("completed");
    let Some(action) = item.get("action") else {
        return status.to_owned();
    };
    serde_json::json!({ "status": status, "action": action }).to_string()
}

fn content_blocks_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.clone(),
            ContentBlock::Thinking { thinking, .. } => thinking.clone(),
            ContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            ContentBlock::ToolCall { name, .. } => format!("[tool_call:{name}]"),
            ContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_images(content: &[ContentBlock]) -> Vec<crate::events::CodingAgentImageContent> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { mime_type, data } => {
                Some(crate::events::CodingAgentImageContent {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn map_delegation_tool_event(
    context: &AgentEventMappingContext,
    tool_call_id: &str,
    tool_name: &str,
    summary: &str,
) -> Option<PromptStreamEvent> {
    if !matches!(tool_name, "delegate_agent" | "delegate_team") {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(summary).ok()?;
    let status = value.get("status")?.as_str()?;
    let target_kind = parse_delegation_target_kind(value.get("target_kind")?.as_str()?)?;
    let target_id = ProfileId::new(value.get("target_id")?.as_str()?.to_owned()).ok()?;
    let requesting_profile_id =
        ProfileId::new(value.get("requesting_profile_id")?.as_str()?.to_owned()).ok()?;
    let task = value.get("task")?.as_str()?.to_owned();

    let context = DelegationEventContext {
        operation_id: context.operation_id.clone(),
        turn_id: context.turn_id.clone(),
        tool_call_id: tool_call_id.to_owned(),
        requesting_profile_id,
        target_kind,
        target_id,
        task,
    };

    match status {
        "requested" => Some(PromptStreamEvent::Delegation(DelegationEvent::Requested {
            context,
        })),
        "rejected" => Some(PromptStreamEvent::Delegation(DelegationEvent::Rejected {
            context,
            reason: value
                .get("message")
                .or_else(|| value.get("error"))
                .and_then(|message| message.as_str())
                .unwrap_or("delegation rejected")
                .to_owned(),
        })),
        _ => None,
    }
}

fn parse_delegation_target_kind(kind: &str) -> Option<ProfileKind> {
    match kind {
        "agent" => Some(ProfileKind::Agent),
        "team" => Some(ProfileKind::Team),
        _ => None,
    }
}

#[cfg(test)]
mod provider_item_tests {
    use super::{AgentEventMappingContext, map_assistant_event};
    use crate::events::prompt_stream::PromptStreamEvent;
    use crate::events::tool::ToolEvent;
    use ai::api::conversation::{AssistantMessage, ContentBlock};
    use ai::api::stream::AssistantMessageEvent;

    fn partial(status: &str) -> AssistantMessage {
        let mut message = AssistantMessage::empty("deepseek-responses", "deepseek-v4-flash");
        message.content.push(ContentBlock::ProviderItem {
            api: "deepseek-responses".into(),
            item: serde_json::json!({
                "type": "web_search_call",
                "id": "web_1",
                "status": status
            }),
        });
        message
    }

    fn partial_with_action(status: &str, action: serde_json::Value) -> AssistantMessage {
        let mut message = partial(status);
        let ContentBlock::ProviderItem { item, .. } =
            &mut message.content[0]
        else {
            panic!("partial must carry a ProviderItem");
        };
        item["action"] = action;
        message
    }

    #[test]
    fn web_search_lifecycle_maps_to_product_tool_events() {
        let context = AgentEventMappingContext::new("op_1", "turn_1");
        let started = map_assistant_event(
            &context,
            &AssistantMessageEvent::ProviderItemStart {
                content_index: 0,
                partial: partial("in_progress"),
            },
        );
        assert!(matches!(
            started.as_slice(),
            [PromptStreamEvent::Tool(ToolEvent::Started {
                tool_call_id,
                name,
                ..
            })] if tool_call_id == "web_1" && name == "web_search"
        ));

        let updated = map_assistant_event(
            &context,
            &AssistantMessageEvent::ProviderItemDelta {
                content_index: 0,
                delta: "searching".into(),
                partial: partial("searching"),
            },
        );
        assert!(matches!(
            updated.as_slice(),
            [PromptStreamEvent::Tool(ToolEvent::Updated { message, .. })]
                if message == "searching"
        ));

        let completed = map_assistant_event(
            &context,
            &AssistantMessageEvent::ProviderItemEnd {
                content_index: 0,
                partial: partial_with_action(
                    "completed",
                    serde_json::json!({"type": "search", "queries": ["DeepSeek API docs"]}),
                ),
            },
        );
        assert!(matches!(
            completed.as_slice(),
            [PromptStreamEvent::Tool(ToolEvent::Completed { summary, .. })]
                if serde_json::from_str::<serde_json::Value>(summary)
                    .is_ok_and(|value| value
                        == serde_json::json!({
                            "status": "completed",
                            "action": {"type": "search", "queries": ["DeepSeek API docs"]}
                        }))
        ));
    }

    #[test]
    fn web_search_completion_without_action_falls_back_to_status() {
        let context = AgentEventMappingContext::new("op_1", "turn_1");
        let completed = map_assistant_event(
            &context,
            &AssistantMessageEvent::ProviderItemEnd {
                content_index: 0,
                partial: partial("completed"),
            },
        );
        assert!(matches!(
            completed.as_slice(),
            [PromptStreamEvent::Tool(ToolEvent::Completed { summary, .. })]
                if summary == "completed"
        ));
    }
}
