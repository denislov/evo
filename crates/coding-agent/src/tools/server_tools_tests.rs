//! Wiring guards for provider-executed tools.
//!
//! `web_search` is declared to the provider but never dispatched locally, so
//! every layer that filters the tool inventory can drop it without any error
//! surfacing — the request simply goes out without the tool and the model
//! answers as if search did not exist. These tests pin the layers that would
//! otherwise fail silently: the model-support gate, the profile grant, the
//! delegated capability release, and the declaration contract.
use agent_core::api::agent::AgentResources;
use ai_protocol::api::model::{Model, ModelCost, ModelInput};

use super::{grant_server_tools, product_tool_ids, server_side_tools, server_tool_ids};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::operations::prompt::context::PromptTurnOptions;
use tool_contract::api::definition::ToolId;

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
    runtime_tool_names_from(api, Vec::new(), true)
}

fn runtime_tool_names_from(
    api: &str,
    tools: Vec<std::sync::Arc<dyn tool_runtime::api::DynamicTool>>,
    register_builtins: bool,
) -> Vec<String> {
    let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(1),
        tools,
        register_builtins,
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
        .all_tool_names()
}

#[test]
fn no_tools_is_not_reopened_by_a_server_side_declaration() {
    // `filter_tools` runs before the runtime snapshot is built, so `--no-tools`
    // reaches this layer as an empty inventory. Re-adding a provider-side tool
    // would reopen a door the caller explicitly shut.
    assert!(runtime_tool_names_from("deepseek-responses", Vec::new(), false).is_empty());
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
    let names = runtime_tool_names("deepseek-responses");
    let declared = names.iter().filter(|name| *name == "web_search").count();
    assert_eq!(declared, 1, "duplicate declaration in {names:?}");
}

#[test]
fn web_search_is_not_a_local_executable() {
    let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model("deepseek-responses"),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(1),
        tools: Vec::new(),
        register_builtins: true,
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
    let runtime = options.runtime().expect("runtime snapshot");
    assert!(
        runtime
            .tools()
            .iter()
            .all(|tool| tool.definition().id.as_str() != "web_search")
    );
    assert_eq!(runtime.provider_tools()[0].id.as_str(), "web_search");
}

#[test]
fn local_builtin_tools_are_registered_only_in_the_typed_runtime_set() {
    let options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model("openai-completions"),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(1),
        tools: Vec::new(),
        register_builtins: true,
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
    let runtime = options.runtime().expect("runtime snapshot");
    assert!(
        runtime
            .tools()
            .iter()
            .all(|tool| !matches!(tool.definition().id.as_str(), "read" | "ls" | "bash"))
    );
    assert_eq!(
        runtime
            .typed_tool_ids()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        [
            "apply_patch",
            "bash",
            "edit",
            "find",
            "grep",
            "hashline_edit",
            "ls",
            "read",
            "web_fetch",
            "write",
        ]
    );
    assert!(runtime.all_tool_names().iter().any(|name| name == "read"));
    assert!(runtime.all_tool_names().iter().any(|name| name == "ls"));
    assert!(runtime.all_tool_names().iter().any(|name| name == "find"));
    assert!(runtime.all_tool_names().iter().any(|name| name == "grep"));
    assert!(runtime.all_tool_names().iter().any(|name| name == "bash"));
}

#[test]
fn server_tools_are_granted_alongside_an_explicit_profile_list() {
    let mut profile_tools = vec![ToolId::new("read").unwrap(), ToolId::new("bash").unwrap()];
    grant_server_tools(&mut profile_tools);
    assert!(profile_tools.iter().any(|id| id.as_str() == "web_search"));
    assert!(profile_tools.iter().any(|id| id.as_str() == "read"));

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
    // IDs absent from the product inventory are filtered out of the capability
    // set and then dropped from the agent inventory without a diagnostic.
    for id in server_tool_ids() {
        assert!(
            product_tool_ids().iter().any(|product| product == &id),
            "{} is not in the capability universe",
            id
        );
    }
}

#[test]
fn web_search_uses_a_provider_only_contract() {
    use tool_contract::api::definition::ToolKind;

    let tools = server_side_tools(&model("deepseek-responses"));
    assert_eq!(tools.len(), 1);
    let definition = &tools[0];
    assert_eq!(definition.kind, ToolKind::WebSearch);
    assert!(definition.capabilities.provider_executed);
    assert!(definition.capabilities.read_only);
    assert!(definition.parameters.is_null());
}

mod delegation {
    use crate::application::capability::OperationCapabilitySnapshot;
    use crate::kernel::capability::{
        ActorId, CapabilityGeneration, CommandCapabilitySet, ToolCapabilitySet,
    };
    use crate::operations::delegation::{
        capability_snapshot_for_delegated_profile, worktree::ChildWorkspaceBinding,
    };
    use crate::profiles::{AgentProfile, DelegationPolicy, ProfileId, ProfileSource};
    use tool_contract::api::definition::ToolId;

    fn parent_with_tools(names: &[&str]) -> OperationCapabilitySnapshot {
        OperationCapabilitySnapshot {
            generation: CapabilityGeneration::new(1),
            operation_id: "parent".into(),
            actor: ActorId::Client,
            model: None,
            tools: ToolCapabilitySet::from_ids(
                names.iter().map(|name| ToolId::new(*name).unwrap()),
            ),
            commands: CommandCapabilitySet::default(),
            workspace: None,
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
            tools: tools
                .iter()
                .map(|name| ToolId::new(*name).unwrap())
                .collect(),
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
            ChildWorkspaceBinding::None,
        )
        .expect("snapshot");
        assert!(released.tools.allows(&ToolId::new("read").unwrap()));
        assert!(
            released.tools.allows(&ToolId::new("web_search").unwrap()),
            "delegate should inherit provider-side search"
        );
        assert!(
            !released.tools.allows(&ToolId::new("bash").unwrap()),
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
            ChildWorkspaceBinding::None,
        )
        .expect("snapshot");
        assert!(
            !released.tools.allows(&ToolId::new("web_search").unwrap()),
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
            ChildWorkspaceBinding::None,
        )
        .expect("snapshot");
        assert!(!released.tools.allows(&ToolId::new("web_search").unwrap()));
        assert!(!released.tools.allows(&ToolId::new("read").unwrap()));
    }
}
