use std::sync::Arc;

use agent_core::api::agent::AgentResources;
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::testing::FauxProvider;

use super::*;
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::events::{CodingAgentProductEventKind, CodingAgentRuntimeProductEvent};
use crate::runtime::facade::{
    CodingAgentOperation, CodingAgentSession, CodingAgentSessionOptions,
    CodingAgentShutdownOutcome, CodingSessionError, PromptTurnOptions,
};

fn model(api: &str) -> Model {
    Model {
        id: "shutdown-lag-model".into(),
        name: "Shutdown Lag Model".into(),
        api: api.into(),
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

fn prompt_options(api: &str, ai_client: ai::api::client::AiClient) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("test".into()),
        max_turns: Some(1),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(ai_client),
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("force real reconnect lag".into()),
    })
}

#[tokio::test]
async fn lagged_reconnect_after_shutdown_recovers_then_delivers_runtime_shutdown_and_closes() {
    let api = "projection-real-shutdown-lag";
    let _provider = crate::test_support::ProviderGuard::register(
        api,
        Arc::new(FauxProvider::simple_text("lagged response")),
    );
    let mut session = CodingAgentSession::non_persistent_with_event_capacities_for_tests(
        CodingAgentSessionOptions::new(),
        1,
        64,
    )
    .await
    .unwrap();
    let async_connection = session
        .connect_internal(CodingAgentClientId::new("async-lag-client"))
        .unwrap();
    let try_connection = session
        .connect_internal(CodingAgentClientId::new("try-lag-client"))
        .unwrap();
    let CodingAgentReconnect::Replayed {
        receiver: mut async_receiver,
        ..
    } = async_connection.reconnect(0).unwrap()
    else {
        panic!("empty cursor must establish async live delivery")
    };
    let CodingAgentReconnect::Replayed {
        receiver: mut try_receiver,
        ..
    } = try_connection.reconnect(0).unwrap()
    else {
        panic!("empty cursor must establish try_recv live delivery")
    };

    session
        .run(CodingAgentOperation::Prompt(prompt_options(
            api,
            _provider.ai_client(),
        )))
        .await
        .unwrap();
    assert_eq!(
        session.shutdown().await.unwrap(),
        CodingAgentShutdownOutcome::ShutDown
    );

    let CodingAgentReconnectDelivery::FreshSnapshotRequired(async_recovery) =
        async_receiver.recv_internal().await.unwrap()
    else {
        panic!("async receiver must take the real lag recovery path")
    };
    let async_boundary = async_recovery.fresh_cursor.last_event_sequence;
    assert_eq!(
        async_recovery.reason,
        CodingAgentRecoveryReason::LiveReceiverLag
    );
    let CodingAgentReconnectDelivery::Event(async_shutdown) =
        async_receiver.recv_internal().await.unwrap()
    else {
        panic!("async receiver must deliver Runtime.ShutDown after recovery")
    };
    assert_eq!(async_shutdown.sequence(), async_boundary);
    assert!(matches!(
        async_shutdown.event(),
        CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
    ));
    assert_eq!(
        async_receiver.recv_internal().await.unwrap_err(),
        CodingSessionError::Cancelled
    );

    let Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(try_recovery)) =
        try_receiver.try_recv_internal().unwrap()
    else {
        panic!("try_recv receiver must take the real lag recovery path")
    };
    let try_boundary = try_recovery.fresh_cursor.last_event_sequence;
    assert_eq!(
        try_recovery.reason,
        CodingAgentRecoveryReason::LiveReceiverLag
    );
    let Some(CodingAgentReconnectDelivery::Event(try_shutdown)) =
        try_receiver.try_recv_internal().unwrap()
    else {
        panic!("try_recv receiver must deliver Runtime.ShutDown after recovery")
    };
    assert_eq!(try_shutdown.sequence(), try_boundary);
    assert!(matches!(
        try_shutdown.event(),
        CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
    ));
    assert_eq!(
        try_receiver.try_recv_internal().unwrap_err(),
        CodingSessionError::Cancelled
    );
}

#[tokio::test]
async fn receiver_detached_before_phase_a_never_enters_shutdown_drain() {
    let mut session = CodingAgentSession::non_persistent_with_event_capacities_for_tests(
        CodingAgentSessionOptions::new(),
        8,
        16,
    )
    .await
    .unwrap();
    let detached = session
        .connect_internal(CodingAgentClientId::new("pre-phase-a-detached"))
        .unwrap();
    let CodingAgentReconnect::Replayed {
        receiver: mut detached_receiver,
        ..
    } = detached.reconnect(0).unwrap()
    else {
        panic!("detached control must start with a live receiver")
    };
    session
        .runtime_host
        .event_hub
        .service
        .emit_diagnostic(None::<String>, "queued before detach");
    assert_eq!(
        detached.detach_internal().unwrap(),
        CodingAgentDetachOutcome::Detached
    );

    let attached = session
        .connect_internal(CodingAgentClientId::new("phase-a-participant"))
        .unwrap();
    let cursor = session
        .runtime_host
        .event_hub
        .service
        .current_product_sequence()
        .get();
    let CodingAgentReconnect::Replayed {
        receiver: mut attached_receiver,
        ..
    } = attached.reconnect(cursor).unwrap()
    else {
        panic!("attached control must establish live delivery")
    };
    session.runtime_shutdown_handle().request_shutdown();
    session.shutdown().await.unwrap();

    assert_eq!(
        detached_receiver.recv_internal().await.unwrap_err(),
        CodingSessionError::Lifecycle {
            reason: crate::runtime::error::CodingAgentLifecycleRejection::Detached
        }
    );
    let Some(CodingAgentReconnectDelivery::Event(shutdown)) =
        attached_receiver.try_recv_internal().unwrap()
    else {
        panic!("Phase-A participant must drain Runtime.ShutDown")
    };
    assert!(matches!(
        shutdown.event(),
        CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
    ));
    assert_eq!(
        attached_receiver.try_recv_internal().unwrap_err(),
        CodingSessionError::Cancelled
    );
}
