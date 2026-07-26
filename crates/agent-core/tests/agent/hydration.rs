use crate::common;

use agent_core::api::agent::{Agent, AgentEvent, AgentMessage};
use ai::api::testing::FauxProvider;
use common::{ProviderGuard, faux_model};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::test]
async fn prompt_starts_after_hydrated_messages() {
    let api = "agent-hydration-history";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::new(vec![common::faux_text_turn(
            "second answer",
        )])),
    );
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.system_prompt = Some("system".into());
    config.max_turns = Some(5);
    let agent = Agent::with_messages(
        config,
        vec![AgentMessage::UserText {
            message_id: "entry001".into(),
            text: "first".into(),
        }],
    );
    let baseline = agent.messages().len();
    let mut stream = agent.prompt("second");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::AgentError { .. }) {
            panic!("unexpected agent error");
        }
    }
    let messages = agent.messages();
    assert_eq!(baseline, 1);
    assert!(matches!(messages[0], AgentMessage::UserText { .. }));
    assert!(messages.len() >= 3);
}
