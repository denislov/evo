use std::sync::{Arc, Mutex};

use ai::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use ai::api::testing::FauxProvider;

use super::support::{self, ProviderGuard};
use crate::app::bootstrap::PromptInvocation;
use crate::events::{
    CodingAgentProductEventKind, CodingAgentRuntimeProductEvent, CodingAgentWorkflowProductEvent,
};
use crate::runtime::facade::{
    CodingAgentClientId, CodingAgentDraftId, CodingAgentOperation, CodingAgentOperationOutcome,
    CodingAgentSession, CodingAgentSessionOptions, CodingAgentShutdownOutcome,
    CodingAgentSubmissionDraft, CodingAgentSubmittedOperationStatus,
};

struct ShutdownGateProvider {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl ApiProvider for ShutdownGateProvider {
    fn stream(&self, model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let started = self.started.lock().unwrap().take();
        let release = self.release.lock().unwrap().take();
        let model_id = model.id.clone();
        Box::pin(async_stream::stream! {
            if let Some(started) = started {
                started.send(()).unwrap();
            }
            if let Some(release) = release {
                release.await.unwrap();
            }
            let mut message = AssistantMessage::empty("shutdown-gate", &model_id);
            message.provider = Some("shutdown-gate".into());
            message.content.push(ContentBlock::Text {
                text: "drained".into(),
                text_signature: None,
            });
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

#[tokio::test]
async fn shutdown_drains_private_runtime_seed_before_publishing_shutdown() {
    let api = "internal-private-seed-shutdown";
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let provider = ProviderGuard::register(
        api,
        Arc::new(ShutdownGateProvider {
            started: Mutex::new(Some(started_tx)),
            release: Mutex::new(Some(release_rx)),
        }),
    );
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new().with_ai_client(provider.ai_client()),
    )
    .await
    .unwrap();
    let shutdown = session.runtime_shutdown_handle();
    let mut events = session.subscribe_product_events_public();
    let prompt = support::prompt_options(
        std::path::Path::new("."),
        api,
        "drain admitted prompt",
        Vec::new(),
        1,
    );

    let running = tokio::spawn(async move {
        let outcome = session.run(CodingAgentOperation::Prompt(prompt)).await;
        (session, outcome)
    });
    started_rx.await.unwrap();
    shutdown.request_shutdown();
    release_tx.send(()).unwrap();

    let (mut session, outcome) = running.await.unwrap();
    assert!(matches!(
        outcome.unwrap(),
        CodingAgentOperationOutcome::Prompt(_)
    ));
    assert_eq!(
        session.shutdown().await.unwrap(),
        CodingAgentShutdownOutcome::ShutDown
    );

    let mut saw_prompt_terminal = false;
    loop {
        let event = events.recv().await.unwrap();
        match event.event() {
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. },
            ) => saw_prompt_terminal = true,
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
                assert!(
                    saw_prompt_terminal,
                    "shutdown must follow the admitted prompt terminal event"
                );
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn prepared_submission_runs_private_seed_and_records_terminal_state() {
    let api = "internal-private-seed-submission";
    let provider = ProviderGuard::register(api, Arc::new(FauxProvider::simple_text("done")));
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new().with_ai_client(provider.ai_client()),
    )
    .await
    .unwrap();
    let connection = session
        .connect(CodingAgentClientId::new("private-seed-client"))
        .unwrap();
    let prompt = support::prompt_options(
        std::path::Path::new("."),
        api,
        "tracked prompt",
        Vec::new(),
        1,
    );
    assert!(matches!(
        prompt.invocation(),
        PromptInvocation::Text(text) if text == "tracked prompt"
    ));
    let prepared = connection
        .prepare_client_submission(
            &mut session,
            Some(CodingAgentSubmissionDraft::new(
                CodingAgentDraftId("private-seed-draft".into()),
                "tracked prompt",
            )),
            CodingAgentOperation::Prompt(prompt),
        )
        .unwrap();

    assert!(matches!(
        prepared.run(&mut session).await.unwrap(),
        CodingAgentOperationOutcome::Prompt(_)
    ));
    let state = connection.state().unwrap();
    assert!(state.drafts.is_empty());
    assert!(matches!(
        state.submitted_operation.unwrap().status,
        CodingAgentSubmittedOperationStatus::Terminal { .. }
    ));
}
