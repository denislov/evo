use crate::internal_tests::support;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_core::api::tool::AgentTool;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use async_stream::stream;
use coding_agent::api::error::CodingAgentPublicDiagnosticSeverity;
use coding_agent::api::operation::{
    CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOptions, PromptTurnOutcome,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};
use support::{EnvGuard, ProviderGuard as RegistryProviderGuard};
use tempfile::tempdir;

#[tokio::test]
async fn default_agent_profile_is_applied_to_prompt_runtime() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/runtime-coder.toml"),
        r#"
schema_version = 1
id = "runtime-coder"
display_name = "Runtime Coder"
model = "claude-haiku-4-5"
system_prompt = "Profile runtime instructions."
tools = ["echo", "missing_tool"]
skills = ["missing_skill"]
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let profile_model = ai::api::model::lookup_model("claude-haiku-4-5").unwrap();
    let fallback_api = "profile-runtime-fallback-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        vec![profile_model.api.clone(), fallback_api.into()],
        calls.clone(),
    );

    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_id("sess_profile_runtime")
            .with_session_log_root(temp.path().join("sessions"))
            .with_default_agent_profile_id("runtime-coder"),
    )
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            fallback_api,
            "use profile",
        )))
        .await
        .unwrap();
    let CodingAgentOperationOutcome::Prompt(outcome) = outcome else {
        panic!("prompt operation returned another outcome")
    };

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.model_id, "claude-haiku-4-5");
    assert_eq!(
        call.context.system_prompt.as_deref(),
        Some("Profile runtime instructions.")
    );
    let tool_names = call
        .context
        .tools
        .as_ref()
        .expect("profile should keep one allowed tool")
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tool_names, vec!["echo"]);
    let PromptTurnOutcome::Success { diagnostics, .. } = outcome else {
        panic!("expected successful prompt outcome: {outcome:#?}");
    };
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == CodingAgentPublicDiagnosticSeverity::Warning
                && diagnostic.summary.contains("missing_tool")
        }),
        "expected missing tool diagnostic, got {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == CodingAgentPublicDiagnosticSeverity::Warning
                && diagnostic.summary.contains("missing_skill")
        }),
        "expected missing skill diagnostic, got {diagnostics:#?}"
    );
}

#[tokio::test]
async fn delegating_agent_profile_exposes_policy_request_tools() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
allow_delegate_team = true
max_depth = 1
allowed_agents = ["coder"]
allowed_teams = ["implementation"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
description = "Implements focused code changes"
"#,
    );
    write_file(
        cwd.join(".evo/teams/implementation.toml"),
        r#"
schema_version = 1
id = "implementation"
display_name = "Implementation Team"
description = "Plans, implements, and reviews changes"
supervisor = "deterministic"
members = ["coder"]
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "profile-runtime-delegation-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(vec![api.into()], calls.clone());

    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_id("sess_profile_delegation_runtime")
            .with_session_log_root(temp.path().join("sessions"))
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "delegate work",
        )))
        .await
        .unwrap();
    let CodingAgentOperationOutcome::Prompt(outcome) = outcome else {
        panic!("prompt operation returned another outcome")
    };

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let tools = calls[0]
        .context
        .tools
        .as_ref()
        .expect("delegation tools should be exposed to provider context");
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"delegate_agent"));
    assert!(tool_names.contains(&"delegate_team"));
    let delegate_agent = tools
        .iter()
        .find(|tool| tool.name == "delegate_agent")
        .unwrap();
    assert_eq!(
        delegate_agent.parameters["properties"]["agent_id"]["enum"],
        serde_json::json!(["coder"])
    );
    assert!(
        delegate_agent
            .description
            .as_deref()
            .unwrap()
            .contains("coder: Coder - Implements focused code changes")
    );
    let delegate_team = tools
        .iter()
        .find(|tool| tool.name == "delegate_team")
        .unwrap();
    assert_eq!(
        delegate_team.parameters["properties"]["team_id"]["enum"],
        serde_json::json!(["implementation"])
    );
    assert!(
        delegate_team.description.as_deref().unwrap().contains(
            "implementation: Implementation Team - Plans, implements, and reviews changes"
        )
    );
    let PromptTurnOutcome::Success { diagnostics, .. } = outcome else {
        panic!("expected successful prompt outcome: {outcome:#?}");
    };
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.summary.contains("is not available yet")),
        "delegation availability warnings should be retired: {diagnostics:#?}"
    );
}

#[tokio::test]
async fn built_in_default_profile_projects_helper_inventory_to_provider_request() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&global).unwrap();
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "profile-runtime-default-delegation-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(vec![api.into()], calls.clone());
    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_id("sess_default_delegation_inventory")
            .with_session_log_root(temp.path().join("sessions")),
    )
    .await
    .unwrap();

    session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "inspect delegation inventory",
        )))
        .await
        .unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let delegate_agent = calls[0]
        .context
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .find(|tool| tool.name == "delegate_agent")
        .expect("built-in default profile should expose delegate_agent");
    assert_eq!(
        delegate_agent.parameters["properties"]["agent_id"]["enum"],
        serde_json::json!(["check", "explore", "review"])
    );
    let description = delegate_agent.description.as_deref().unwrap();
    for expected in ["check: Check", "explore: Explore", "review: Review"] {
        assert!(
            description.contains(expected),
            "provider-visible description is missing {expected:?}: {description}"
        );
    }
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

#[derive(Debug, Clone)]
struct RecordedCall {
    model_id: String,
    context: Context,
}

struct RecordingProvider {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl ApiProvider for RecordingProvider {
    fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        self.calls.lock().unwrap().push(RecordedCall {
            model_id: model.id.clone(),
            context: ctx,
        });
        let model_id = model.id.clone();
        Box::pin(stream! {
            let mut message = AssistantMessage::empty("profile-runtime-test", &model_id);
            message.provider = Some("profile-runtime-test".into());
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
}
