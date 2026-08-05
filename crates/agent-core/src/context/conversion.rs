use crate::agent::tool_adapter::contract_tool_declaration;
use crate::agent::types::{AgentMessage, AgentResources};
use crate::execution::capture::bash_execution_to_text;
use crate::resources::system_prompt::format_skills_for_system_prompt;
use ai_protocol::api::conversation::{ContentBlock, Context, Message, Tool};
use tool_contract::api::definition::ToolDefinition;

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
    runtime_tools: &[ToolDefinition],
    provider_tools: &[ToolDefinition],
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

    let llm_tools: Option<Vec<Tool>> = if runtime_tools.is_empty() && provider_tools.is_empty() {
        None
    } else {
        let mut declarations = runtime_tools
            .iter()
            .map(contract_tool_declaration)
            .collect::<Vec<_>>();
        declarations.extend(provider_tools.iter().map(contract_tool_declaration));
        Some(declarations)
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
    runtime_tools: &[ToolDefinition],
    provider_tools: &[ToolDefinition],
    resources: &AgentResources,
) -> Context {
    let llm_messages = default_convert_to_llm(messages, resources);
    assemble_context(
        system_prompt,
        messages,
        llm_messages,
        runtime_tools,
        provider_tools,
        resources,
    )
}
