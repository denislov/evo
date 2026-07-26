use crate::internal_tests::support;

use std::sync::Arc;

use crate::app::bootstrap::PromptInvocation;
use crate::app::prompt_runtime::PromptRuntimeOptions;
use agent_core::api::resources::AgentResources;
use ai::api::testing::FauxProvider;
use coding_agent::api::client::{
    CodingAgentClientId, CodingAgentSubmittedOperationStatus, CodingAgentSubmittedTerminalAnchor,
};
use coding_agent::api::event::{
    CodingAgentProductEventKind, CodingAgentSessionProductEvent,
    CodingAgentSubmittedEventDurability,
};
use coding_agent::api::operation::{
    CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOptions,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};
use support::ProviderGuard;

fn options(api: &str, invocation: PromptInvocation) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: support::model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("test".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: None,
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation,
    })
}

async fn seeded_compaction_session(
    api: &str,
    session_id: &str,
    root: &std::path::Path,
    ai_client: ai::api::client::AiClient,
) -> CodingAgentSession {
    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(ai_client)
            .with_session_id(session_id)
            .with_session_log_root(root),
    )
    .await
    .unwrap();
    let outcome = session
        .run(CodingAgentOperation::Prompt(options(
            api,
            PromptInvocation::Text("seed question".into()),
        )))
        .await
        .unwrap();
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
    session
}

#[tokio::test]
async fn terminal_association_uses_the_exact_compact_root_event() {
    let api = "operation-association-compact";
    let _provider = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("seed answer", ai::api::conversation::StopReason::Stop),
            FauxProvider::text_call("compact summary", ai::api::conversation::StopReason::Stop),
        ])),
    );
    let temp = tempfile::tempdir().unwrap();
    let mut session =
        seeded_compaction_session(api, "sess_association", temp.path(), _provider.ai_client())
            .await;
    let connection = session
        .connect(CodingAgentClientId::new("association-client"))
        .unwrap();
    let operation = CodingAgentOperation::Compact(options(
        api,
        PromptInvocation::Compact {
            custom_instructions: None,
        },
    ));
    let prepared = connection
        .prepare_client_submission(&mut session, None, operation)
        .unwrap();

    assert!(matches!(
        prepared.run(&mut session).await.unwrap(),
        CodingAgentOperationOutcome::Compact(_)
    ));

    let submitted = connection
        .state()
        .unwrap()
        .submitted_operation
        .expect("compact terminal state");
    let sequence = match submitted.status {
        CodingAgentSubmittedOperationStatus::Terminal {
            anchor:
                CodingAgentSubmittedTerminalAnchor::ProductEvent {
                    sequence,
                    durability: CodingAgentSubmittedEventDurability::Durable,
                },
            ..
        } => sequence,
        other => panic!("unexpected compact terminal anchor: {other:?}"),
    };
    let coding_agent::api::client::CodingAgentReconnect::Replayed { events, .. } =
        connection.reconnect(0).unwrap()
    else {
        panic!("compact events should be retained")
    };
    let matching = events
        .iter()
        .filter(|event| {
            event.sequence() == sequence
                && matches!(
                    event.event(),
                    CodingAgentProductEventKind::Session(
                        CodingAgentSessionProductEvent::CompactionCompleted { .. }
                    )
                )
        })
        .count();
    assert_eq!(matching, 1, "anchor must identify the one Compact root");
}
