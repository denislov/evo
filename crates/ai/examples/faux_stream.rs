use ai::api::provider::ProviderRegistry;
use ai::api::provider::faux::{FauxProvider, FauxResponse};
use ai_protocol::api::conversation::{ContentBlock, Context, Message};
use ai_protocol::api::model::{Model, ModelCost, ModelInput};
use ai_protocol::api::stream::AssistantMessageEvent;
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let provider = Arc::new(FauxProvider::new(vec![FauxResponse {
        text_deltas: vec![
            "Thinking step-by-step...\n".into(),
            "The answer ".into(),
            "is 42.".into(),
        ],
        thinking_deltas: vec![],
        tool_calls: vec![],
    }]));
    let registry = ProviderRegistry::new();
    registry.register("faux-api", provider);

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

    let ctx = Context {
        system_prompt: Some("Answer concisely.".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "What is the meaning of life?".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };

    let mut stream = registry.stream_model(&model, ctx, None);

    println!("=== faux provider streaming demo ===\n");
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                print!("[think] {}", delta);
            }
            AssistantMessageEvent::ToolcallDelta { delta, .. } => {
                print!("[tool: {}]", delta);
            }
            AssistantMessageEvent::Done { message, .. } => {
                println!("\n\n--- Done ---");
                println!("stop reason: {:?}", message.stop_reason);
                println!("usage: {:?}", message.usage);
            }
            AssistantMessageEvent::Error { message, .. } => {
                eprintln!(
                    "\nError: {}",
                    message.error_message.as_deref().unwrap_or("unknown error")
                );
            }
            _ => {}
        }
    }
    println!("=== end ===");
}
