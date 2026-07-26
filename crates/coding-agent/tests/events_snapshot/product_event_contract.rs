use crate::internal_tests::support;

use std::sync::Arc;

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use agent_core::api::resources::AgentResources;
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::testing::FauxProvider;
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDurability, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalOperationKind, CodingAgentProductEventTerminalStatus,
    CodingAgentSessionProductEvent, CodingAgentWorkflowProductEvent,
};
use coding_agent::api::operation::{CodingAgentOperation, PromptTurnOptions};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};
use support::ProviderGuard;

struct ContractFixture {
    persistent: Vec<CodingAgentProductEvent>,
    non_persistent: Vec<CodingAgentProductEvent>,
}

#[tokio::test]
async fn public_receiver_preserves_typed_order_and_metadata() {
    let fixture = run_contract_fixture().await;
    assert_monotonic(&fixture.persistent);
    assert_monotonic(&fixture.non_persistent);

    let pending = fixture
        .persistent
        .iter()
        .find(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WritePending { .. }
                )
            )
        })
        .expect("persistent prompt should announce its pending write");
    let committed = fixture
        .persistent
        .iter()
        .find(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WriteCommitted { .. }
                )
            )
        })
        .expect("persistent prompt should announce its committed write");
    let skipped = fixture
        .non_persistent
        .iter()
        .find(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WriteSkipped { .. }
                )
            )
        })
        .expect("non-persistent prompt should announce its skipped write");

    assert!(
        matches!(pending.durability(), CodingAgentProductEventDurability::PendingSessionWrite { operation_id } if Some(operation_id.as_str()) == pending.operation_id())
    );
    assert!(matches!(
        committed.durability(),
        CodingAgentProductEventDurability::Durable { .. }
    ));
    assert_eq!(pending.capability_generation(), Some(1));
    assert_eq!(committed.capability_generation(), Some(1));
    assert_eq!(committed.session_id(), Some("sess_product_event_contract"));
    assert_eq!(skipped.capability_generation(), Some(1));
    assert_eq!(
        skipped.durability(),
        &CodingAgentProductEventDurability::LiveOnly
    );

    assert_eq!(
        committed.terminal_status(),
        Some(CodingAgentProductEventTerminalStatus::Completed)
    );
    assert_eq!(committed.terminal_operation(), None);

    let prompt_terminal = fixture
        .persistent
        .iter()
        .find(|event| {
            matches!(
                event.event(),
                CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptCompleted { .. }
                )
            )
        })
        .expect("prompt should publish a root terminal event");
    assert_eq!(prompt_terminal.operation_id(), pending.operation_id());
    assert_eq!(prompt_terminal.parent_operation_id(), None);
    assert_eq!(prompt_terminal.root_operation_id(), pending.operation_id());
    assert_eq!(prompt_terminal.session_id(), None);
    assert_eq!(prompt_terminal.capability_generation(), Some(1));
    assert_eq!(
        prompt_terminal.terminal_operation().unwrap().kind,
        CodingAgentProductEventTerminalOperationKind::Prompt
    );

    let json = serde_json::to_value(prompt_terminal).unwrap();
    assert_eq!(json["event"]["family"], "workflow");
    assert_eq!(json["event"]["payload"]["kind"], "prompt_completed");
    assert_eq!(
        json["operation_id"],
        prompt_terminal.operation_id().unwrap()
    );
    assert_eq!(json["terminal_status"], "completed");
    assert_eq!(json["capability_generation"], 1);
    assert_eq!(json["terminal_operation"]["kind"], "prompt");
    assert_eq!(json["durability"]["state"], "live_only");
}

async fn run_contract_fixture() -> ContractFixture {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let api = "product-event-contract-faux";
    let _provider = ProviderGuard::register(api, Arc::new(FauxProvider::simple_text("done")));

    let mut persistent_session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider.ai_client())
            .with_cwd(&cwd)
            .with_session_id("sess_product_event_contract")
            .with_session_log_root(temp.path().join("sessions")),
    )
    .await
    .unwrap();
    let mut persistent_receiver = persistent_session.subscribe_product_events_public();
    persistent_session
        .run(CodingAgentOperation::Prompt(prompt_options(api, &cwd)))
        .await
        .unwrap();
    let persistent = drain(&mut persistent_receiver);

    let mut non_persistent_session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider.ai_client())
            .with_cwd(&cwd),
    )
    .await
    .unwrap();
    let mut non_persistent_receiver = non_persistent_session.subscribe_product_events_public();
    non_persistent_session
        .run(CodingAgentOperation::Prompt(prompt_options(api, &cwd)))
        .await
        .unwrap();
    let non_persistent = drain(&mut non_persistent_receiver);

    ContractFixture {
        persistent,
        non_persistent,
    }
}

fn drain(
    receiver: &mut coding_agent::api::event::CodingAgentProductEventReceiver,
) -> Vec<CodingAgentProductEvent> {
    let mut events = Vec::new();
    while let Some(event) = receiver.try_recv().unwrap() {
        events.push(event);
    }
    events
}

fn assert_monotonic(events: &[CodingAgentProductEvent]) {
    assert!(!events.is_empty());
    for pair in events.windows(2) {
        assert_eq!(pair[1].sequence(), pair[0].sequence() + 1);
    }
}

fn prompt_options(api: &str, cwd: &std::path::Path) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(3),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(cwd.to_path_buf())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hello".into()),
    })
}

fn model(api: &str) -> Model {
    Model {
        id: "product-event-contract-model".into(),
        name: "Product Event Contract Model".into(),
        api: api.into(),
        provider: "test".into(),
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
        context_window: 8_192,
        max_tokens: 1_024,
        headers: None,
        compat: None,
    }
}
