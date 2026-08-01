use agent_core::api::agent::{Agent, AgentConfig, AgentEvent, AgentMessage};
use agent_core::api::tool::{AgentTool, AgentToolOutput};
use ai::api::client::AiClient;
use ai::api::conversation::{ContentBlock, StopReason};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::faux::FauxProvider;
use ai::api::stream::AssistantMessageEvent;
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let provider = Arc::new(FauxProvider::with_call_queue(vec![
        FauxProvider::text_call("I'll search for that.", StopReason::ToolUse),
        FauxProvider::text_call("Done searching. The answer is 42.", StopReason::Stop),
    ]));
    let ai_client = Arc::new(AiClient::new());
    ai_client.register_provider("faux-api", provider);

    let model = Model {
        id: "faux-model".into(),
        name: "Faux Model".into(),
        api: "faux-api".into(),
        provider: "faux".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    };

    let mut config = AgentConfig::new(model);
    config.system_prompt = Some("You are a helpful assistant.".into());
    config.max_turns = Some(5);
    config.provider_streamer = Some(Arc::new({
        let ai_client = Arc::clone(&ai_client);
        move |model, context, options| ai_client.stream_model(model, context, options)
    }));

    let agent = Agent::new(config);

    agent
        .add_tool(AgentTool {
            kind: Default::default(),
            name: "search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            execution_mode: None,
            execute: Arc::new(|_context, _args, _on_update| {
                Box::pin(async move {
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "search results: 42 is the answer".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .expect("example tool definition must be valid");

    println!("=== agent-core loop example ===\n");

    let mut stream = agent.prompt("What is the meaning of life?");
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TurnStart { turn } => {
                println!("--- Turn {} ---", turn);
            }
            AgentEvent::BeforeProviderRequest { .. } => {}
            AgentEvent::LlmEvent(e) => {
                if let AssistantMessageEvent::TextDelta { delta, .. } = &e {
                    print!("{}", delta);
                }
            }
            AgentEvent::ToolCallStart { tool_name, .. } => {
                println!("\n[tool call: {}]", tool_name);
            }
            AgentEvent::ToolCallUpdate { update, .. } => {
                println!("[tool update: {:?}]", update.content);
            }
            AgentEvent::ToolCallEnd { result, .. } => {
                if result.is_error {
                    println!("[tool error: {:?}]", result.content);
                } else {
                    println!("[tool result: {:?}]", result.content);
                }
            }
            AgentEvent::AgentDone { message } => {
                println!("\n\nDone — stop reason: {:?}", message.stop_reason);
            }
            AgentEvent::AgentError { error } => {
                eprintln!("\nError: {}", error);
            }
            AgentEvent::SessionCompacted { summary, .. } => {
                println!("\n[compacted: {}]", summary);
            }
        }
    }

    println!("\n=== Final messages ({}) ===", agent.messages().len());
    for msg in agent.messages() {
        match msg {
            AgentMessage::UserText { text, .. } => println!("  User: {}", text),
            AgentMessage::Assistant { .. } => {
                println!("  Assistant (response)")
            }
            AgentMessage::ToolResult { tool_call_id, .. } => {
                println!("  ToolResult: {}", tool_call_id)
            }
            AgentMessage::SystemPrompt { text, .. } => {
                println!("  System: {}", text)
            }
            AgentMessage::CompactionSummary { summary, .. } => {
                println!("  Compaction: {}", summary)
            }
            AgentMessage::BashExecution { command, .. } => {
                println!("  BashExecution: {}", command)
            }
            AgentMessage::Custom { custom_type, .. } => {
                println!("  Custom: {}", custom_type)
            }
            AgentMessage::BranchSummary { from_id, .. } => {
                println!("  BranchSummary from {}", from_id)
            }
        }
    }
}
