//! Dispatch routing and error-fidelity guards.
//!
//! [`CodingAgentSession::run_internal`] selects a dispatcher purely from
//! `descriptor.dispatch_mode`. These tests drive a real session through all
//! three dispatch families and assert that each one both reaches its runner and
//! surfaces the runner's own error rather than masking it as a routing failure.
//! CAG-311 removes the defensive error arms from the handlers; these tests are
//! what prove the removal changed nothing observable.
use std::sync::Arc;

use agent_core::api::agent::AgentResources;
use ai::api::conversation::StopReason;
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::faux::FauxProvider;

use super::OperationDispatchMode;
use super::contract::{CodingAgentOperation, CodingAgentOperationOutcome};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::runtime::error::CodingSessionError;
use crate::runtime::facade::{CodingAgentSession, CodingAgentSessionOptions};
use crate::test_support::ProviderGuard;

fn model(api: &str) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
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

fn prompt_options(api: &str, prompt: &str) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text(prompt.into()),
    })
}

/// Drives one operation from each dispatch family through the real router on a
/// real persistent session. Every family must reach its runner and produce its
/// own outcome variant.
#[tokio::test]
async fn run_internal_routes_every_dispatch_family_to_its_runner() {
    let api = "coding-session-dispatch-family-routing";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("async answer", StopReason::Stop),
        ])),
    );
    let temp = tempfile::tempdir().unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id("sess_dispatch_family_routing")
            .with_session_log_root(temp.path()),
    )
    .await
    .unwrap();

    // Async family.
    let async_operation = CodingAgentOperation::Prompt(prompt_options(api, "async prompt"));
    assert_eq!(
        async_operation.descriptor().dispatch_mode,
        OperationDispatchMode::Async
    );
    let async_outcome = session.run_internal(async_operation).await.unwrap();
    assert!(
        matches!(async_outcome, CodingAgentOperationOutcome::Prompt(_)),
        "async family must produce a prompt outcome, got {async_outcome:?}"
    );

    // Sync read-only family.
    let read_only_operation = CodingAgentOperation::ExportCurrent;
    assert_eq!(
        read_only_operation.descriptor().dispatch_mode,
        OperationDispatchMode::SyncReadOnly
    );
    let read_only_outcome = session.run_internal(read_only_operation).await.unwrap();
    assert!(
        matches!(read_only_outcome, CodingAgentOperationOutcome::Export(_)),
        "sync read-only family must produce an export outcome, got {read_only_outcome:?}"
    );

    // Sync mutable family.
    let sync_mut_operation = CodingAgentOperation::SetSessionName {
        name: Some("routed".into()),
    };
    assert_eq!(
        sync_mut_operation.descriptor().dispatch_mode,
        OperationDispatchMode::SyncMutable
    );
    let sync_mut_outcome = session.run_internal(sync_mut_operation).await.unwrap();
    assert!(
        matches!(
            sync_mut_outcome,
            CodingAgentOperationOutcome::SessionNameChanged { .. }
        ),
        "sync mutable family must produce a session-name outcome, got {sync_mut_outcome:?}"
    );
}

/// A sync-mutable operation that its runner rejects must surface the runner's
/// own error. The message must stay the session-writer capability message, not
/// become a dispatcher-selection error.
#[tokio::test]
async fn sync_mutable_runner_errors_are_not_masked_as_routing_failures() {
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();

    let error = session
        .run_internal(CodingAgentOperation::SetSessionName {
            name: Some("no durable session".into()),
        })
        .await
        .expect_err("a non-persistent session cannot commit a durable name change");

    let CodingSessionError::UnsupportedCapability { capability } = &error else {
        panic!("SetSessionName must fail on the missing session writer, got {error:?}");
    };
    assert_eq!(
        capability, "session names require a persistent Rust-native session",
        "the sync-mutable handler must surface the runner's capability error"
    );
    assert_dispatch_error_is_not_a_routing_fallback(&error, "SetSessionName");
}

/// Same guarantee for the async family: a compact request whose invocation the
/// runner rejects must preserve the runner's typed input error.
#[tokio::test]
async fn async_runner_errors_are_not_masked_as_routing_failures() {
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();

    let error = session
        .run_internal(CodingAgentOperation::Compact(PromptTurnOptions::new(
            PromptInvocation::Text("compact without a compaction invocation".into()),
        )))
        .await
        .expect_err("compact requires a compaction invocation");

    let CodingSessionError::Input { message } = &error else {
        panic!("Compact must fail on its own input validation, got {error:?}");
    };
    assert_eq!(
        message, "compact operation requires a compaction invocation",
        "the async handler must surface the runner's input error"
    );
    assert_dispatch_error_is_not_a_routing_fallback(&error, "Compact");
}

/// The removed routing fallback used the phrase "requires ... dispatcher".
/// Any such error means the runner's own error was masked during dispatch.
fn assert_dispatch_error_is_not_a_routing_fallback(error: &CodingSessionError, name: &str) {
    if let CodingSessionError::UnsupportedCapability { capability } = error {
        assert!(
            !capability.contains("dispatcher"),
            "{name}: router sent the operation to the wrong dispatcher: {capability}"
        );
    }
}
