//! Wiring guards for provider-executed tools.
//!
//! `web_search` is declared to the provider but never dispatched locally, so
//! every layer that filters the tool inventory can drop it without any error
//! surfacing — the request simply goes out without the tool and the model
//! answers as if search did not exist. These tests pin the layers that would
//! otherwise fail silently: the model-support gate, the profile grant, the
//! delegated capability release, and the outgoing wire shape.
use agent_core::api::agent::AgentResources;
use ai::api::model::{Model, ModelCost, ModelInput};

use super::{PRODUCT_TOOL_NAMES, SERVER_TOOL_NAMES, grant_server_tools, server_side_tools};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::operations::prompt::context::PromptTurnOptions;

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

fn runtime_tool_names(api: &str) -> Vec<String> {
    runtime_tool_names_from(
        api,
        super::builtin_tools(".".into()).expect("builtin tools"),
    )
}

fn runtime_tool_names_from(api: &str, tools: Vec<agent_core::api::tool::AgentTool>) -> Vec<String> {
    let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(1),
        tools,
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hi".into()),
    });
    options
        .runtime()
        .expect("runtime snapshot")
        .tools()
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

#[test]
fn no_tools_is_not_reopened_by_a_server_side_declaration() {
    // `filter_tools` runs before the runtime snapshot is built, so `--no-tools`
    // reaches this layer as an empty inventory. Re-adding a provider-side tool
    // would reopen a door the caller explicitly shut.
    assert!(runtime_tool_names_from("deepseek-responses", Vec::new()).is_empty());
}

#[test]
fn web_search_is_declared_only_for_models_whose_api_supports_it() {
    assert!(
        runtime_tool_names("deepseek-responses")
            .iter()
            .any(|name| name == "web_search")
    );
    assert!(
        runtime_tool_names("openai-responses")
            .iter()
            .any(|name| name == "web_search")
    );
    // No opt-in flag: an unsupported API degrades to no web search rather than
    // to a request the provider would reject.
    assert!(
        !runtime_tool_names("openai-completions")
            .iter()
            .any(|name| name == "web_search")
    );
    assert!(
        !runtime_tool_names("anthropic-messages")
            .iter()
            .any(|name| name == "web_search")
    );
}

#[test]
fn declaring_the_same_runtime_twice_does_not_duplicate_the_tool() {
    // `add_tool` and `apply_tool_policy` both reject duplicate names, so a
    // restore path that already carries the declaration must not gain a second.
    let names = runtime_tool_names("deepseek-responses");
    let declared = names.iter().filter(|name| *name == "web_search").count();
    assert_eq!(declared, 1, "duplicate declaration in {names:?}");
}

#[test]
fn server_tools_are_granted_alongside_an_explicit_profile_list() {
    let mut profile_tools = vec!["read".to_owned(), "bash".to_owned()];
    grant_server_tools(&mut profile_tools);
    assert!(profile_tools.iter().any(|name| name == "web_search"));
    assert!(profile_tools.iter().any(|name| name == "read"));

    // Idempotent: a profile that already names it gains nothing.
    let before = profile_tools.len();
    grant_server_tools(&mut profile_tools);
    assert_eq!(profile_tools.len(), before);
}

#[test]
fn an_explicitly_empty_tool_list_stays_empty() {
    // An empty list is a deliberate "no tools" configuration. Granting network
    // reach into it would punch through a fail-closed setting.
    let mut profile_tools = Vec::new();
    grant_server_tools(&mut profile_tools);
    assert!(profile_tools.is_empty());
}

#[test]
fn server_tools_are_in_the_product_capability_universe() {
    // Names absent from `PRODUCT_TOOL_NAMES` are filtered out of the capability
    // set and then dropped from the agent inventory without a diagnostic.
    for name in SERVER_TOOL_NAMES {
        assert!(
            PRODUCT_TOOL_NAMES.contains(&name),
            "{name} is not in the capability universe"
        );
    }
}

#[test]
fn declared_web_search_reaches_the_deepseek_wire_as_a_server_tool() {
    use ai::api::conversation::{Context, Tool};

    let tools = server_side_tools(&model("deepseek-responses"));
    assert_eq!(tools.len(), 1);

    // Mirrors `assemble_context`: the agent tool's kind is what carries the
    // server-side intent onto the wire.
    let context = Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: Some(
            tools
                .iter()
                .map(|tool| Tool {
                    kind: tool.kind,
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    parameters: tool.parameters.clone(),
                })
                .collect(),
        ),
    };
    let value = serde_json::to_value(&context).expect("context serializes");
    assert_eq!(value["tools"][0]["type"], "web_search");
}

mod delegation {
    use crate::application::capability::OperationCapabilitySnapshot;
    use crate::kernel::capability::{
        ActorId, CapabilityGeneration, CommandCapabilitySet, ToolCapabilitySet,
    };
    use crate::operations::delegation::capability_snapshot_for_delegated_profile;
    use crate::profiles::{AgentProfile, DelegationPolicy, ProfileId, ProfileSource};

    fn parent_with_tools(names: &[&str]) -> OperationCapabilitySnapshot {
        OperationCapabilitySnapshot {
            generation: CapabilityGeneration::new(1),
            operation_id: "parent".into(),
            actor: ActorId::Client,
            model: None,
            tools: ToolCapabilitySet::from_names(names.iter().map(|name| (*name).to_owned())),
            commands: CommandCapabilitySet::default(),
            filesystem: None,
            shell: None,
            session_read: None,
            session_write: None,
            ui: None,
        }
    }

    fn profile(tools: &[&str]) -> AgentProfile {
        AgentProfile {
            schema_version: 1,
            id: ProfileId::new("child").expect("valid profile id"),
            display_name: "Child".into(),
            description: None,
            model: None,
            system_prompt: None,
            tools: tools.iter().map(|name| (*name).to_owned()).collect(),
            skills: Vec::new(),
            supervision: Default::default(),
            delegation: DelegationPolicy::default(),
            source: ProfileSource::BuiltIn,
            path: None,
        }
    }

    #[test]
    fn a_delegate_inherits_web_search_without_naming_it() {
        let parent = parent_with_tools(&["read", "bash", "web_search"]);
        let released = capability_snapshot_for_delegated_profile(
            &parent,
            "child-op",
            &profile(&["read"]),
            ActorId::Client,
        );
        assert!(released.tools.allows("read"));
        assert!(
            released.tools.allows("web_search"),
            "delegate should inherit provider-side search"
        );
        assert!(
            !released.tools.allows("bash"),
            "profile did not ask for bash"
        );
    }

    #[test]
    fn a_delegate_cannot_reach_wider_than_its_parent() {
        let parent = parent_with_tools(&["read"]);
        let released = capability_snapshot_for_delegated_profile(
            &parent,
            "child-op",
            &profile(&["read"]),
            ActorId::Client,
        );
        assert!(
            !released.tools.allows("web_search"),
            "a parent without web_search must not hand it to a child"
        );
    }

    #[test]
    fn a_delegate_granted_no_tools_gains_nothing() {
        let parent = parent_with_tools(&["read", "web_search"]);
        let released = capability_snapshot_for_delegated_profile(
            &parent,
            "child-op",
            &profile(&[]),
            ActorId::Client,
        );
        assert!(!released.tools.allows("web_search"));
        assert!(!released.tools.allows("read"));
    }
}
