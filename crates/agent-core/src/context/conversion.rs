use crate::agent::types::{AgentMessage, AgentResources, AgentTool};
use crate::execution::capture::bash_execution_to_text;
use crate::resources::system_prompt::format_skills_for_system_prompt;
use ai::api::conversation::{ContentBlock, Context, Message, Tool};

/// Convert `AgentMessage`s into the LLM-facing `Message` list. Mirrors TS
/// `convertToLlm` (`pi/packages/agent/src/harness/messages.ts`). The harness
/// can replace this step via the `convert_to_llm` hook; see
/// [`crate::hooks::ConvertToLlmHook`].
pub fn default_convert_to_llm(
    messages: &[AgentMessage],
    _resources: &AgentResources,
) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|msg| match msg {
            AgentMessage::UserText { text, .. } => Some(Message::User {
                content: vec![ContentBlock::Text {
                    text: text.clone(),
                    text_signature: None,
                }],
            }),
            AgentMessage::Assistant { message, .. } => Some(Message::Assistant {
                content: message.content.clone(),
            }),
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                tool_name,
                is_error,
                ..
            } => Some(Message::ToolResult {
                tool_call_id: tool_call_id.clone(),
                tool_name: Some(tool_name.clone()),
                is_error: Some(*is_error),
                content: content.clone(),
            }),
            AgentMessage::SystemPrompt { .. } => None,
            AgentMessage::CompactionSummary { summary, .. } => Some(Message::User {
                content: vec![ContentBlock::Text {
                    text: format!(
                        "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{}\n</summary>",
                        summary
                    ),
                    text_signature: None,
                }],
            }),
            AgentMessage::BashExecution {
                command,
                output,
                exit_code,
                cancelled,
                truncated,
                full_output_path,
                exclude_from_context,
                ..
            } => {
                if *exclude_from_context {
                    None
                } else {
                    Some(Message::User {
                        content: vec![ContentBlock::Text {
                            text: bash_execution_to_text(
                                command,
                                output,
                                *exit_code,
                                *cancelled,
                                *truncated,
                                full_output_path.as_deref(),
                            ),
                            text_signature: None,
                        }],
                    })
                }
            }
            AgentMessage::Custom { content, .. } => Some(Message::User {
                content: content.clone(),
            }),
            AgentMessage::BranchSummary { summary, .. } => Some(Message::User {
                content: vec![ContentBlock::Text {
                    text: format!(
                        "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{}\n</summary>",
                        summary
                    ),
                    text_signature: None,
                }],
            }),
        })
        .collect()
}

/// Build the final `Context` from already-converted LLM `messages`. Handles the
/// system prompt resolution and tool list construction. Use together with
/// `default_convert_to_llm` (or a custom hook output) to produce the LLM
/// request payload.
pub fn assemble_context(
    system_prompt: &Option<String>,
    agent_messages: &[AgentMessage],
    llm_messages: Vec<Message>,
    tools: &[AgentTool],
    resources: &AgentResources,
) -> Context {
    let system = {
        let configured = system_prompt.clone();
        let from_messages = agent_messages.iter().find_map(|m| match m {
            AgentMessage::SystemPrompt { text, .. } => Some(text.clone()),
            _ => None,
        });
        let base = configured.or(from_messages);

        if !resources.skills.is_empty() {
            let skills_block = format_skills_for_system_prompt(&resources.skills);
            if !skills_block.is_empty() {
                match base {
                    Some(ref b) => Some(format!("{}\n\n{}", b, skills_block)),
                    None => Some(skills_block),
                }
            } else {
                base
            }
        } else {
            base
        }
    };

    let llm_tools: Option<Vec<Tool>> = if tools.is_empty() {
        None
    } else {
        Some(
            tools
                .iter()
                .map(|t| Tool {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: t.parameters.clone(),
                })
                .collect(),
        )
    };

    Context {
        system_prompt: system,
        messages: llm_messages,
        tools: llm_tools,
    }
}

pub fn convert_to_context(
    system_prompt: &Option<String>,
    messages: &[AgentMessage],
    tools: &[AgentTool],
    resources: &AgentResources,
) -> Context {
    let llm_messages = default_convert_to_llm(messages, resources);
    assemble_context(system_prompt, messages, llm_messages, tools, resources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_msg() -> ai::api::conversation::AssistantMessage {
        ai::api::conversation::AssistantMessage::empty("test", "test-model")
    }

    #[test]
    fn user_text_becomes_user_message() {
        let msgs = vec![AgentMessage::UserText {
            message_id: "1".into(),
            text: "hello".into(),
        }];
        let ctx = convert_to_context(&None, &msgs, &[], &AgentResources::default());
        assert_eq!(ctx.messages.len(), 1);
        match &ctx.messages[0] {
            Message::User { content } => match &content[0] {
                ContentBlock::Text { text, .. } => assert_eq!(text, "hello"),
                _ => panic!("expected text block"),
            },
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn assistant_passthrough() {
        let am = assistant_msg();
        let msgs = vec![AgentMessage::Assistant {
            message_id: "2".into(),
            message: am.clone(),
        }];
        let ctx = convert_to_context(&None, &msgs, &[], &AgentResources::default());
        assert_eq!(ctx.messages.len(), 1);
        match &ctx.messages[0] {
            Message::Assistant { content } => {
                assert_eq!(*content, am.content);
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn tool_result_becomes_tool_result_message() {
        let msgs = vec![AgentMessage::ToolResult {
            message_id: "3".into(),
            tool_call_id: "call_1".into(),
            tool_name: "test_tool".into(),
            is_error: false,
            content: vec![ContentBlock::Text {
                text: "result".into(),
                text_signature: None,
            }],
        }];
        let ctx = convert_to_context(&None, &msgs, &[], &AgentResources::default());
        assert_eq!(ctx.messages.len(), 1);
        match &ctx.messages[0] {
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(content.len(), 1);
            }
            _ => panic!("expected tool result message"),
        }
    }

    #[test]
    fn system_prompt_from_config() {
        let ctx = convert_to_context(
            &Some("be helpful".into()),
            &[],
            &[],
            &AgentResources::default(),
        );
        assert_eq!(ctx.system_prompt, Some("be helpful".into()));
    }

    #[test]
    fn system_prompt_from_messages() {
        let msgs = vec![AgentMessage::SystemPrompt {
            message_id: "4".into(),
            text: "be concise".into(),
        }];
        let ctx = convert_to_context(&None, &msgs, &[], &AgentResources::default());
        assert_eq!(ctx.system_prompt, Some("be concise".into()));
    }

    #[test]
    fn config_system_prompt_wins_over_messages() {
        let msgs = vec![AgentMessage::SystemPrompt {
            message_id: "4".into(),
            text: "from messages".into(),
        }];
        let ctx = convert_to_context(
            &Some("from config".into()),
            &msgs,
            &[],
            &AgentResources::default(),
        );
        assert_eq!(ctx.system_prompt, Some("from config".into()));
    }

    #[test]
    fn tools_converted_to_llm_tools() {
        let tools = vec![AgentTool {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: std::sync::Arc::new(|_, _, _on_update| {
                Box::pin(async { Ok(crate::agent::types::AgentToolOutput::new(vec![])) })
            }),
        }];
        let ctx = convert_to_context(&None, &[], &tools, &AgentResources::default());
        let llm_tools = ctx.tools.unwrap();
        assert_eq!(llm_tools.len(), 1);
        assert_eq!(llm_tools[0].name, "search");
        assert_eq!(llm_tools[0].description, Some("search the web".into()));
    }

    #[test]
    fn empty_tools_produce_none() {
        let ctx = convert_to_context(&None, &[], &[], &AgentResources::default());
        assert!(ctx.tools.is_none());
    }
}
