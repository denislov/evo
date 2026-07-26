#![allow(deprecated)]

use crate::internal_tests::support;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_core::api::tool::AgentTool;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use async_stream::stream;
use coding_agent::api::error::CodingAgentErrorCategory;
use coding_agent::api::event::{CodingAgentProductEvent, CodingAgentProductEventReceiver};
use coding_agent::api::operation::{
    AgentInvocationOptions, CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOptions,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};
use support::{EnvGuard, ProviderGuard as RegistryProviderGuard};
use tempfile::tempdir;

#[tokio::test]
async fn one_off_agent_invocation_uses_target_profile_runtime_policy() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/runtime-coder.toml"),
        r#"
schema_version = 1
id = "runtime-coder"
display_name = "Runtime Coder"
model = "claude-haiku-4-5"
system_prompt = "Profile invocation instructions."
tools = ["echo"]
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let profile_model = ai::api::model::lookup_model("claude-haiku-4-5").unwrap();
    let fallback_api = "agent-invocation-fallback-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        vec![profile_model.api.clone(), fallback_api.into()],
        calls.clone(),
    );

    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_id("sess_agent_invocation")
            .with_session_log_root(temp.path().join("sessions")),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::InvokeAgent(
            AgentInvocationOptions::new(
                "runtime-coder",
                "implement the task",
                prompt_options(&cwd, fallback_api, "implement the task"),
            ),
        ))
        .await
        .unwrap();
    let outcome = extract_agent_invocation(outcome);

    assert_eq!(outcome.profile_id.as_str(), "runtime-coder");
    assert_eq!(outcome.final_text, "profile applied");
    assert_ne!(outcome.operation_id, outcome.child_operation_id);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.model_id, "claude-haiku-4-5");
    assert_eq!(
        call.context.system_prompt.as_deref(),
        Some("Profile invocation instructions.")
    );
    let tool_names = call
        .context
        .tools
        .as_ref()
        .expect("profile should keep the allowed tool")
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tool_names, vec!["echo"]);

    let events = drain_events(&mut events);
    assert!(has_event(&events, "Agent(InvocationStarted)"));
    assert!(has_event(&events, "Agent(InvocationCompleted)"));
}

#[tokio::test]
async fn one_off_agent_invocation_uses_task_over_prompt_options_invocation() {
    let temp = tempdir().unwrap();
    let api = "agent-invocation-task-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(vec![api.into()], calls.clone());
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(temp.path()),
    )
    .await
    .unwrap();

    session
        .run(CodingAgentOperation::InvokeAgent(
            AgentInvocationOptions::new(
                "default",
                "delegated task",
                prompt_options(temp.path(), api, "parent prompt"),
            ),
        ))
        .await
        .unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(user_texts(&calls[0].context), vec!["delegated task"]);
}

#[tokio::test]
async fn one_off_agent_invocation_does_not_commit_parent_session_transcript() {
    let temp = tempdir().unwrap();
    let api = "agent-invocation-no-commit-api";
    let _provider_guard =
        ProviderGuard::register(vec![api.into()], Arc::new(Mutex::new(Vec::new())));
    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_session_id("sess_agent_invocation_no_commit")
            .with_session_log_root(temp.path().join("sessions")),
    )
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::InvokeAgent(
            AgentInvocationOptions::new(
                "default",
                "do not persist",
                prompt_options(temp.path(), api, "do not persist"),
            ),
        ))
        .await
        .unwrap();
    let outcome = extract_agent_invocation(outcome);

    assert_eq!(outcome.final_text, "profile applied");
    let export = extract_export(
        session
            .run(CodingAgentOperation::ExportCurrent)
            .await
            .unwrap(),
    );
    assert!(
        export.transcript.is_empty(),
        "one-off agent invocation must not write parent transcript: {export:#?}"
    );
}

#[tokio::test]
async fn one_off_agent_invocation_emits_single_failed_event_for_child_failure() {
    let temp = tempdir().unwrap();
    let api = "agent-invocation-child-failure-api";
    let _provider_guard = ProviderGuard::register_failing(vec![api.into()]);
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(temp.path()),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let error = session
        .run(CodingAgentOperation::InvokeAgent(
            AgentInvocationOptions::new(
                "default",
                "fail child",
                prompt_options(temp.path(), api, "fail child"),
            ),
        ))
        .await
        .unwrap_err();

    support::assert_public_error(&error, CodingAgentErrorCategory::Provider, "provider", true);
    assert!(
        !serde_json::to_string(&error)
            .unwrap()
            .contains("child failed")
    );
    let events = drain_events(&mut events);
    let failure_count = events
        .iter()
        .filter(|event| event.kind_name() == "invocation_failed")
        .count();
    assert_eq!(
        failure_count, 1,
        "expected one invocation failure event: {events:#?}"
    );
}

#[tokio::test]
async fn one_off_agent_invocation_rejects_unknown_profile_with_product_event() {
    let temp = tempdir().unwrap();
    let api = "agent-invocation-missing-profile-api";
    let _provider_guard =
        ProviderGuard::register(vec![api.into()], Arc::new(Mutex::new(Vec::new())));
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(temp.path()),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let error = session
        .run(CodingAgentOperation::InvokeAgent(
            AgentInvocationOptions::new(
                "missing",
                "task",
                prompt_options(temp.path(), api, "task"),
            ),
        ))
        .await
        .unwrap_err();

    support::assert_public_error(&error, CodingAgentErrorCategory::Input, "input", false);
    assert!(!serde_json::to_string(&error).unwrap().contains("missing"));
    let events = drain_events(&mut events);
    assert!(has_event(&events, "Agent(InvocationFailed)"));
}

fn prompt_options(cwd: &Path, api: &str, prompt: &str) -> PromptTurnOptions {
    support::prompt_options(cwd, api, prompt, vec![echo_tool(), extra_tool()], 2)
}

fn echo_tool() -> AgentTool {
    AgentTool::new_text(
        "echo",
        "echoes input",
        serde_json::json!({"type": "object"}),
        |_context, _args| async { Ok("echo".to_owned()) },
    )
}

fn extra_tool() -> AgentTool {
    AgentTool::new_text(
        "extra",
        "extra tool",
        serde_json::json!({"type": "object"}),
        |_context, _args| async { Ok("extra".to_owned()) },
    )
}

fn write_file(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn drain_events(receiver: &mut CodingAgentProductEventReceiver) -> Vec<CodingAgentProductEvent> {
    let mut events = Vec::new();
    while let Ok(Some(event)) = receiver.try_recv() {
        events.push(event);
    }
    events
}

fn has_event(events: &[CodingAgentProductEvent], kind: &str) -> bool {
    let (expected_family, expected) = kind
        .rsplit_once('(')
        .map(|(family, value)| (Some(family), value.trim_end_matches(')')))
        .unwrap_or((None, kind));
    let expected_family = expected_family.map(pascal_to_snake);
    events.iter().any(|event| {
        expected_family
            .as_deref()
            .is_none_or(|family| event.family_typed().as_str() == family)
            && event.kind_name() == pascal_to_snake(expected)
    })
}

fn pascal_to_snake(value: &str) -> String {
    value
        .chars()
        .enumerate()
        .flat_map(|(index, character)| {
            (index > 0 && character.is_ascii_uppercase())
                .then_some('_')
                .into_iter()
                .chain(std::iter::once(character.to_ascii_lowercase()))
        })
        .collect()
}

fn extract_agent_invocation(
    outcome: CodingAgentOperationOutcome,
) -> coding_agent::api::operation::AgentInvocationOutcome {
    match outcome {
        CodingAgentOperationOutcome::AgentInvocation(value) => value,
        other => panic!("expected agent invocation outcome, got {other:?}"),
    }
}

fn extract_export(
    outcome: CodingAgentOperationOutcome,
) -> coding_agent::api::view::CodingAgentSessionExport {
    match outcome {
        CodingAgentOperationOutcome::Export(value) => value,
        other => panic!("expected export outcome, got {other:?}"),
    }
}

fn user_texts(context: &Context) -> Vec<String> {
    context
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct RecordedCall {
    model_id: String,
    context: Context,
}

struct RecordingProvider {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

struct FailingProvider;

impl ApiProvider for FailingProvider {
    fn stream(&self, model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let model_id = model.id.clone();
        Box::pin(stream! {
            let mut message = AssistantMessage::empty("agent-invocation-failure", &model_id);
            message.error_message = Some("child failed".into());
            message.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                message,
            };
        })
    }
}

impl ApiProvider for RecordingProvider {
    fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        self.calls.lock().unwrap().push(RecordedCall {
            model_id: model.id.clone(),
            context: ctx,
        });
        let model_id = model.id.clone();
        Box::pin(stream! {
            let mut message = AssistantMessage::empty("agent-invocation-test", &model_id);
            message.provider = Some("agent-invocation-test".into());
            message.content.push(ContentBlock::Text {
                text: "profile applied".into(),
                text_signature: None,
            });
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

struct ProviderGuard {
    _guard: RegistryProviderGuard,
}

impl ProviderGuard {
    fn ai_client(&self) -> ai::api::client::AiClient {
        self._guard.ai_client()
    }

    fn register(apis: Vec<String>, calls: Arc<Mutex<Vec<RecordedCall>>>) -> Self {
        let providers = apis
            .into_iter()
            .map(|api| {
                (
                    api,
                    Arc::new(RecordingProvider {
                        calls: calls.clone(),
                    }) as Arc<dyn ApiProvider>,
                )
            })
            .collect();
        Self {
            _guard: RegistryProviderGuard::register_many(providers),
        }
    }

    fn register_failing(apis: Vec<String>) -> Self {
        let providers = apis
            .into_iter()
            .map(|api| (api, Arc::new(FailingProvider) as Arc<dyn ApiProvider>))
            .collect();
        Self {
            _guard: RegistryProviderGuard::register_many(providers),
        }
    }
}
