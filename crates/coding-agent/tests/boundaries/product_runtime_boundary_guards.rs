//! Product runtime ownership and bypass prevention boundaries.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMethod {
    name: String,
    visibility: &'static str,
    test_only: bool,
    attributes: Vec<String>,
    body: String,
    file: String,
    line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, Copy)]
struct MethodExpectation {
    name: &'static str,
    group: &'static str,
    visibility: &'static str,
    test_only: bool,
}

#[test]
fn tui_edge_is_confined_to_terminal_adapter_migration_inventory() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for path in rust_files_under(&scan.crate_root.join("src")) {
        let source = fs::read_to_string(&path).expect("read production Rust source");
        if !source.contains("tui::") {
            continue;
        }
        let relative = relative_path(&scan.repo_root, &path);
        violations.push(relative);
    }

    assert!(
        violations.is_empty(),
        "tui references escaped the frozen terminal migration inventory:\n{}",
        violations.join("\n")
    );

    for product_root in ["src/config", "src/resources", "src/theme"] {
        for path in rust_files_under(&scan.crate_root.join(product_root)) {
            let source = fs::read_to_string(&path).expect("read product-owned source");
            assert!(
                !source.contains("tui::"),
                "{product_root} must remain terminal-type-free: {}",
                relative_path(&scan.repo_root, &path)
            );
        }
    }
}

#[test]
fn cli_model_list_consumes_the_product_catalog_without_a_lower_runtime_edge() {
    let scan = SourceScan::new();
    let path = scan.crate_root.join("../cli/src/cli/list_models.rs");
    let source = fs::read_to_string(&path).expect("read model-list source");

    assert!(
        source.contains("CodingAgentModelCatalogEntry")
            && source.contains("model_catalog")
            && !source.contains("ai::")
            && !source.contains("agent_core::"),
        "the migration-period model-list adapter must consume only the safe product catalog"
    );
}

#[test]
fn product_startup_paths_do_not_depend_on_the_cli_parser_dto() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for path in rust_files_under(&scan.crate_root.join("src")) {
        let relative = relative_path(&scan.repo_root, &path);
        if relative.contains("/src/internal_tests/") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read product source");
        let production = production_source(&sanitize_rust_source(&source));
        if production.contains("CliArgs") {
            violations.push(relative);
        }
    }

    assert!(
        violations.is_empty(),
        "CLI parser DTO escaped into product startup/runtime ownership:\n{}",
        violations.join("\n")
    );

    let invocation = fs::read_to_string(scan.crate_root.join("src/app/invocation.rs"))
        .expect("read invocation options source");
    for forbidden in [
        "ai::",
        "Model,",
        "AiClient",
        "Vec<AgentTool",
        "AgentResources",
        "Config",
        "AuthStore",
        "ResolvedSessionTarget",
        "SessionRunOptions",
    ] {
        assert!(
            !invocation.contains(forbidden),
            "public invocation input must not expose runtime authority: {forbidden}"
        );
    }
}

#[test]
fn terminal_model_presentation_does_not_consume_provider_models() {
    let scan = SourceScan::new();
    for relative in [
        "../cli/src/interactive/model_selector.rs",
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/commands.rs",
    ] {
        let path = scan.crate_root.join(relative);
        let source = fs::read_to_string(&path).expect("read terminal model presentation source");
        assert!(
            !source.contains("ai::"),
            "{relative} must use the safe product model catalog instead of provider model types"
        );
    }
}

#[test]
fn terminal_presentation_leaf_modules_use_only_the_categorized_product_facade() {
    let scan = SourceScan::new();
    for relative in [
        "../cli/src/interactive/slash.rs",
        "../cli/src/interactive/profile_menu.rs",
        "../cli/src/interactive/model_selector.rs",
        "../cli/src/interactive/tree_selector.rs",
        "../cli/src/interactive/delegation_confirmation_menu.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read terminal presentation leaf source");
        for forbidden in [
            "crate::app::",
            "crate::runtime::",
            "crate::authorization::",
            "crate::events::",
            "ai::",
            "agent_core::",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} must consume product contracts through crate::api before the atomic cli move: {forbidden}"
            );
        }
    }
}

#[test]
fn terminal_operation_startup_uses_the_opaque_product_factory() {
    let scan = SourceScan::new();
    let app = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/app.rs"))
        .expect("read interactive bootstrap source");
    let prompt_context = app
        .split("pub(super) struct PromptContext {")
        .nth(1)
        .and_then(|source| source.split("\n}\n\nimpl PromptContext").next())
        .expect("PromptContext body");
    for forbidden in [
        "ai::",
        "agent_core::api::tool::AgentTool",
        "AgentResources",
        "ProviderAuthDiagnostic",
        "AiClient",
        "Model,",
        "SessionRunOptions",
        "ResolvedSessionTarget",
        "AuthStore",
        "invocation_api_key",
        "session_name",
    ] {
        assert!(
            !prompt_context.contains(forbidden),
            "interactive PromptContext must not expose private runtime authority: {forbidden}"
        );
    }
    assert!(
        prompt_context.contains("CodingAgentOperationFactory")
            && prompt_context.contains("CodingAgentSessionBootstrap")
            && prompt_context.contains("CodingAgentAuthController")
            && prompt_context.contains("cwd: PathBuf"),
        "interactive PromptContext must retain opaque product factories/bootstrap plus safe cwd presentation"
    );

    let loop_source = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/loop.rs"))
        .expect("read interactive loop source");
    let production = production_source(&sanitize_rust_source(&loop_source));
    for forbidden in [
        "PromptRuntimeOptions {",
        "PromptTurnOptions::from_prompt_runtime_options",
        "prompt_context.prompt_options(",
        "model_repair_prompt_options",
        "prompt_context.api_key",
        "prompt_context.auth_diagnostics",
        "prompt_context.ai_client",
        "prompt_context.tools",
        "prompt_context.resources",
        "prompt_context.model.clone()",
        "prompt_context.session.as_ref()",
        "prompt_context.session_target",
        "prompt_context.session_name",
    ] {
        assert!(
            !production.contains(forbidden),
            "terminal operation startup must not reconstruct private runtime state: {forbidden}"
        );
    }
    for required in [
        "prompt_context.prepared_prompt_operation(",
        "prompt_context.resource_prompt_operation(",
        "prompt_context.agent_invocation_operation(",
        "prompt_context.team_invocation_operation(",
        "prompt_context.compact_operation(",
        "prompt_context.self_healing_edit_operation(",
        "prompt_context.fork_session_operation(",
        "prompt_context.branch_summary_operation(",
        "prompt_context.session_bootstrap()",
    ] {
        assert!(
            production.contains(required),
            "terminal operation startup must delegate typed construction to the product facade: {required}"
        );
    }

    let prompt_task = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/prompt_task.rs"),
    )
    .expect("read interactive prompt task source");
    let prompt_task_production = production_source(&sanitize_rust_source(&prompt_task));
    for forbidden in [
        "PromptRuntimeOptions",
        "PromptTurnOptions::from_prompt_runtime_options",
        "SessionRunOptions",
        "ResolvedSessionTarget",
        "open_interactive_session",
        ".open_internal()",
        ".connect_internal(",
        ".run_internal(",
        "prepare_client_submission_internal",
        ".discard_internal(",
        "crate::runtime::",
        "crate::authorization::",
        "_internal",
        "session.run(",
        "handoff_interactive_connection",
    ] {
        assert!(
            !prompt_task_production.contains(forbidden),
            "PromptTask production must consume only categorized product APIs plus adapter-local error/channel types: {forbidden}"
        );
    }
    for required in [
        "coding_agent::api::authorization",
        "coding_agent::api::client",
        "coding_agent::api::operation",
        "coding_agent::api::runtime",
        "CodingAgentOperation",
        "CodingAgentSessionBootstrap",
        "CodingAgentPromptControl",
        "CodingAgentOperationControl",
        "open_task_session(",
        "prepare_interactive_submission(",
        ".operation_id()",
        "acknowledge_outcome(",
        ".open()",
        ".connect(",
        ".run(",
        "prepare_client_submission(",
    ] {
        assert!(
            prompt_task_production.contains(required),
            "PromptTask must preserve public client-scoped operation/control choreography: {required}"
        );
    }
}

#[test]
fn terminal_event_projection_consumes_only_public_client_snapshots() {
    let scan = SourceScan::new();
    let sources = [
        (
            "loop",
            "../cli/src/interactive/loop.rs",
            [
                "coding_agent::api::client",
                ".connect(",
                "connection.snapshot.clone()",
                ".acknowledge(",
                ".reconnect_from_cursor(",
            ]
            .as_slice(),
        ),
        (
            "event bridge",
            "../cli/src/interactive/event_bridge.rs",
            [
                "coding_agent::api::client",
                "CodingAgentClientProjection",
                "CodingAgentContextSnapshot",
                "CodingAgentSnapshot",
                "product.apply(event)",
            ]
            .as_slice(),
        ),
        (
            "root",
            "../cli/src/interactive/root.rs",
            [
                "coding_agent::api::client",
                "CodingAgentContextSnapshot",
                "CodingAgentOperationSnapshot",
                "CodingAgentSnapshot",
            ]
            .as_slice(),
        ),
    ];

    for (owner, relative, required) in sources {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .unwrap_or_else(|error| panic!("read {owner} source: {error}"));
        let production = production_source(&sanitize_rust_source(&source));
        for forbidden in [
            "crate::runtime::client",
            "crate::runtime::facade",
            "UiSnapshot",
            "UiContextProjection",
            "initial_ui_state",
            ".ui_state(",
            ".connect_internal(",
            ".reconnect_internal(",
            ".acknowledge_internal(",
            ".recv_internal(",
            ".try_recv_internal(",
            ".detach_internal(",
        ] {
            let matches = production
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(forbidden))
                .map(|(index, line)| format!("{}: {}", index + 1, line.trim()))
                .collect::<Vec<_>>();
            assert!(
                matches.is_empty(),
                "terminal {owner} production must not consume private projection/client state: {forbidden}\n{}",
                matches.join("\n")
            );
        }
        for required in required {
            assert!(
                production.contains(required),
                "terminal {owner} must retain public client projection choreography: {required}"
            );
        }
    }
}

#[test]
fn terminal_startup_and_theme_consume_only_resolved_product_projections() {
    let scan = SourceScan::new();
    let adapter_sources = [
        "../cli/src/interactive/app.rs",
        "../cli/src/interactive/loop.rs",
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/render.rs",
        "../cli/src/interactive/syntax.rs",
        "../cli/src/interactive/theme.rs",
    ];

    for relative in adapter_sources {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        let production = production_source(&sanitize_rust_source(&source));
        for forbidden in [
            "crate::theme",
            "crate::resources",
            "crate::config",
            "ThemeJson",
            "ThemeResource",
            "ThemeReloadSignal",
            "ThemeWatcher::start",
        ] {
            assert!(
                !production.contains(forbidden),
                "terminal startup/theme production in {relative} must not consume private product theme/configuration state: {forbidden}"
            );
        }
    }

    let app = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/app.rs"))
        .expect("read terminal app source");
    let app_production = production_source(&sanitize_rust_source(&app));
    for forbidden in [
        "resolve_application_context_from_options",
        "configured_model_choices",
        "rotation_model_choices",
        "resolve_profile_registry",
        "resource_command_catalog",
    ] {
        assert!(
            !app_production.contains(forbidden),
            "terminal app must consume the resolved startup bundle instead of rebuilding product authority: {forbidden}"
        );
    }
    for required in [
        "CodingAgentInteractiveStartup",
        "CodingAgentThemeSnapshot",
        "tui_theme_from_snapshot",
    ] {
        assert!(
            app_production.contains(required),
            "terminal app must retain the public startup/theme projection: {required}"
        );
    }

    let loop_source = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/loop.rs"))
        .expect("read terminal loop source");
    let loop_production = production_source(&sanitize_rust_source(&loop_source));
    let startup_entry = loop_source
        .split("pub(super) async fn run_interactive_loop_with_input")
        .nth(1)
        .and_then(|source| source.split("fn initialize_started_tui").next())
        .expect("production interactive startup entry");
    for forbidden in [
        "CliArgs",
        "CliRunOptions",
        "ApplicationRunOptions",
        "terminal_mode_from_config",
    ] {
        assert!(
            !startup_entry.contains(forbidden),
            "terminal loop must start from the public resolved bundle: {forbidden}"
        );
    }
    for required in [
        "CodingAgentInteractiveStartup",
        ".theme_controller",
        ".watch(",
        "CodingAgentThemeSnapshot",
    ] {
        assert!(
            loop_production.contains(required),
            "terminal loop must retain public startup/theme choreography: {required}"
        );
    }
}

#[test]
fn terminal_thinking_presentation_uses_the_product_contract() {
    let scan = SourceScan::new();
    let core_type = "agent_core::api::agent::ThinkingLevel";
    for relative in [
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/slash.rs",
        "../cli/src/interactive/model_selector.rs",
        "../cli/src/interactive/commands.rs",
    ] {
        let path = scan.crate_root.join(relative);
        let source = fs::read_to_string(&path).expect("read terminal thinking presentation source");
        assert!(
            !source.contains(core_type),
            "{relative} must not consume the core thinking-level type"
        );
    }

    for relative in [
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/slash.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read typed terminal thinking presentation source");
        assert!(
            source.contains("CodingAgentThinkingLevel"),
            "{relative} must consume the product-owned thinking-level contract"
        );
    }

    let app_path = scan.crate_root.join("../cli/src/interactive/app.rs");
    let app_source = fs::read_to_string(app_path).expect("read terminal bootstrap source");
    let prompt_context = app_source
        .split("pub(super) struct PromptContext {")
        .nth(1)
        .and_then(|source| source.split("\n}\n\nimpl PromptContext").next())
        .expect("PromptContext body");
    assert!(
        !prompt_context.contains(core_type),
        "terminal bootstrap must not convert the product thinking contract"
    );

    let factory = fs::read_to_string(scan.crate_root.join("src/app/operation_factory.rs"))
        .expect("read product operation factory");
    assert!(
        factory.contains("CodingAgentThinkingLevel")
            && factory.contains("ThinkingLevel")
            && factory.contains("thinking_level.map(ThinkingLevel::from)"),
        "the product operation factory must own the lower-runtime thinking conversion"
    );
}

#[test]
fn terminal_resource_commands_do_not_consume_lower_resource_records() {
    let scan = SourceScan::new();
    for relative in [
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/commands.rs",
        "../cli/src/interactive/slash.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read terminal resource command source");
        assert!(
            !source.contains("agent_core::api::resources"),
            "{relative} must consume product resource-command DTOs instead of lower resource records"
        );
    }

    let root = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/root.rs"))
        .expect("read terminal root source");
    assert!(
        root.contains("CodingAgentResourceCommand")
            && !root.contains("Vec<agent_core::api::resources::Skill>")
            && !root.contains("Vec<agent_core::api::resources::PromptTemplate>"),
        "terminal root must retain only safe resource-command presentation metadata"
    );

    let commands = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/commands.rs"))
        .expect("read terminal command source");
    for forbidden in [
        "format_skill_invocation",
        "substitute_args",
        "expand_skill_command",
        "expand_prompt_template",
    ] {
        assert!(
            !commands.contains(forbidden),
            "terminal command dispatch must submit typed product invocations, not expand resource content: {forbidden}"
        );
    }

    let loop_source = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/loop.rs"))
        .expect("read terminal loop source");
    assert!(
        !loop_source.contains("prompt_context.resources.skills")
            && !loop_source.contains("prompt_context.resources.prompt_templates"),
        "terminal startup presentation must consume the safe resource-command catalog"
    );
}

#[test]
fn terminal_session_navigation_uses_product_queries_and_authority_free_views() {
    let scan = SourceScan::new();
    for relative in [
        "../cli/src/interactive/session_actions.rs",
        "../cli/src/interactive/session_selector.rs",
        "../cli/src/interactive/tree_selector.rs",
        "../cli/src/interactive/root.rs",
        "../cli/src/interactive/commands.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read terminal session presentation source");
        for forbidden in [
            "agent_core::api::transcript",
            "CodingAgentSessionHydration",
            "SessionLogStore",
            "SessionService",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} must not consume lower session records or storage authority: {forbidden}"
            );
        }
    }

    let root = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/root.rs"))
        .expect("read terminal root source");
    assert!(
        root.contains("CodingAgentSessionQuery")
            && root.contains("session_query")
            && !root.contains("active_session_path"),
        "terminal root must retain an opaque product query handle and authority-free choices"
    );

    let tree = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/tree_selector.rs"),
    )
    .expect("read terminal tree selector source");
    assert!(
        tree.contains("CodingAgentSessionTreeNode")
            && tree.contains("CodingAgentSessionTreeRole")
            && !tree.contains("SessionEntry")
            && !tree.contains("StoredAgentMessage"),
        "terminal tree presentation must consume only the product tree projection"
    );

    let session_actions = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/session_actions.rs"),
    )
    .expect("read terminal session action source");
    assert!(
        session_actions.contains("CodingAgentSessionBootstrap")
            && session_actions.contains(".selected_snapshot()")
            && !session_actions.contains("hydrate_interactive_session_target("),
        "terminal hydration must consume the opaque session bootstrap instead of private options/targets"
    );
}

#[test]
fn extension_wasm_and_generic_flow_implementations_cannot_return() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("coding-agent crate should be in the workspace");
    let manifest =
        fs::read_to_string(crate_root.join("Cargo.toml")).expect("read coding-agent manifest");
    assert!(
        !manifest.contains("mlua"),
        "Lua runtime dependency must stay deleted"
    );
    assert!(
        !manifest.contains("wasmtime"),
        "Wasmtime dependency must stay deleted"
    );

    let forbidden_extension_symbols = [
        "ExtensionPlatformOwner",
        "ExtensionToolExecutor",
        "ExtensionToolRegistry",
        "WorkspaceActivationSnapshot",
        "CodingAgentExtensionActivation",
        "CodingAgentPluginLoadOutcome",
        "Operation::PluginLoad",
        "OperationKind::PluginLoad",
        "wasmtime::",
        "PluginRegistry",
        "PluginSource",
        "ToolProvider",
        "CommandProvider",
        "HookProvider",
        "UiProvider",
        "KeybindProvider",
        "LuaToolProvider",
        "LuaCommandProvider",
    ];
    let forbidden_flow_symbols = [
        "agent_core::api::flow",
        "crate::flow",
        "FlowNode",
        "FlowOutcome",
        "FlowRunOptions",
        "FlowService",
        "AgentTurnFlow",
    ];

    let mut violations = Vec::new();
    for root in [
        crate_root.join("src"),
        repo_root.join("crates/agent-core/src"),
    ] {
        for path in rust_files_under(&root) {
            let source = fs::read_to_string(&path).expect("read production Rust source");
            for forbidden in forbidden_extension_symbols
                .iter()
                .chain(forbidden_flow_symbols.iter())
            {
                if source.contains(forbidden) {
                    violations.push(format!(
                        "{} contains {forbidden}",
                        relative_path(repo_root, &path)
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Extension/Wasm or generic Flow code returned:\n{}",
        violations.join("\n")
    );

    for removed in [
        "crates/agent-core/src/flow",
        "crates/coding-agent/src/services/flow.rs",
        "crates/coding-agent/src/services/plugin.rs",
        "crates/coding-agent/src/extensions",
        "crates/coding-agent/src/contributions",
        "crates/coding-agent/src/operations/plugin_load",
        "contracts/extensions",
        "sdk/typescript",
        "scripts/extension-contracts.sh",
        "scripts/extension-runtime.sh",
        "scripts/extension-sdk.sh",
        "tools/architecture-prototypes",
        "docs/0.6.0-extension-tool-contribution-plan.md",
        "docs/0.6.x-sandboxed-extension-productization-roadmap.md",
        "docs/architecture/extension-platform.md",
        "docs/architecture/extension-tool-contract-0.6.0.md",
    ] {
        assert!(
            !repo_root.join(removed).exists(),
            "removed path returned: {removed}"
        );
    }
    for operation in [
        "agent_invocation",
        "branch_summary",
        "compaction",
        "export",
        "prompt",
        "self_healing_edit",
        "team_invocation",
    ] {
        assert!(
            !crate_root
                .join(format!("src/operations/{operation}/flow.rs"))
                .exists(),
            "typed operation must not regain a Flow wrapper: {operation}"
        );
    }
}

#[test]
fn reload_remains_a_local_resource_command_without_plugin_load_semantics() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let slash = fs::read_to_string(crate_root.join("../cli/src/interactive/slash.rs")).unwrap();
    let commands =
        fs::read_to_string(crate_root.join("../cli/src/interactive/commands.rs")).unwrap();
    let event_loop = fs::read_to_string(crate_root.join("../cli/src/interactive/loop.rs")).unwrap();
    let rpc = fs::read_to_string(crate_root.join("../cli/src/rpc/commands.rs")).unwrap();

    assert!(slash.contains("name: \"reload\".into()"));
    assert!(slash.contains("Reload local configuration and resources"));
    assert!(commands.contains("\"reload\" => handle_reload_command(root)"));
    assert!(commands.contains("InteractiveAction::ReloadResources"));
    assert!(event_loop.contains("prompt_context.reload()"));
    assert!(event_loop.contains("root.apply_prompt_context(prompt_context)"));
    assert!(event_loop.contains("before reloading local resources"));
    assert!(!event_loop.contains("PluginLoad"));
    assert!(!rpc.contains("Reload"));
    assert!(!rpc.contains("PluginLoad"));
}

#[test]
fn extension_host_api_directory_stays_deleted() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!crate_root.join("src/extensions").exists());
}

#[test]
fn extension_contract_and_contribution_scaffolding_stay_deleted() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root.parent().and_then(Path::parent).unwrap();
    assert!(!crate_root.join("src/contributions").exists());
    assert!(!repo_root.join("contracts/extensions").exists());
    assert!(!repo_root.join("sdk/typescript").exists());
}

#[test]
fn extension_runtime_dependencies_stay_absent() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml")).unwrap();
    for dependency in ["wasmtime", "wat = ", "semver = "] {
        assert!(
            !manifest.contains(dependency),
            "Extension runtime dependency returned: {dependency}"
        );
    }
}

#[test]
fn lower_layers_remain_free_of_product_extension_types() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_root = crate_root.join("../agent-core/src");

    let mut stack = vec![core_root];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).expect("read agent-core source directory") {
            let path = entry.expect("read agent-core source entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let source = fs::read_to_string(&path).expect("read agent-core source");
                for forbidden in [
                    "ExtensionTool",
                    "ExtensionHandlerRef",
                    "WorkspaceActivationSnapshot",
                    "PluginLoad",
                    "wasmtime::",
                ] {
                    assert!(
                        !source.contains(forbidden),
                        "agent-core learned product Extension type {forbidden} in {}",
                        path.display()
                    );
                }
            }
        }
    }
}

#[test]
fn session_store_failure_controls_remain_test_only() {
    let scan = SourceScan::new();
    let store_path = scan.crate_root.join("src/session/repository.rs");
    let source = fs::read_to_string(&store_path).expect("read session store source");
    let sanitized = sanitize_rust_source(&source);

    for signature in [
        "failures: Arc<Mutex<StoreFailureState>>",
        "pub(crate) enum StoreFailurePoint",
        "struct StoreFailureState",
        "pub(crate) fn fail_after(",
        "fn fail_if_injected(",
    ] {
        assert_eq!(
            sanitized.matches(signature).count(),
            1,
            "session store test control must exist exactly once: {signature}"
        );
        assert_direct_cfg_test(&sanitized, signature);
    }

    for point in [
        "CreateBlobs",
        "CreateIndex",
        "WriteManifest",
        "CreateEventLog",
        "AppendEvents",
        "AppendOutbox",
        "UpdateManifest",
        "RemoveSession",
    ] {
        let call = format!("self.fail_if_injected(StoreFailurePoint::{point})?");
        assert_eq!(
            sanitized.matches(&call).count(),
            1,
            "expected exactly one directly gated failure call for {point}"
        );
        assert_direct_cfg_test(&sanitized, &call);
    }

    let test_support_source =
        fs::read_to_string(scan.crate_root.join("src/runtime/facade/test_support.rs"))
            .expect("read coding session test-support source");
    let session_sanitized = sanitize_rust_source(&test_support_source);
    for signature in [
        "pub(crate) fn arm_append_events_failure_for_tests(",
        "pub(crate) fn arm_update_manifest_failure_for_tests(",
        "pub(crate) fn queue_pending_delegation_for_tests(",
    ] {
        assert_eq!(
            session_sanitized.matches(signature).count(),
            1,
            "owner-local test bridge must exist exactly once: {signature}"
        );
        assert_direct_cfg_test(&session_sanitized, signature);
    }

    let mut violations = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src/runtime")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source = fs::read_to_string(&path).expect("read coding-session source");
        let sanitized = sanitize_rust_source(&source);
        for (index, line) in sanitized.lines().enumerate() {
            let trimmed = line.trim();
            let fault_name = trimmed.contains("fail_after")
                || trimmed.contains("StoreFailurePoint")
                || trimmed.contains("StoreFailureState")
                || ((trimmed.contains("inject") || trimmed.contains("Injection"))
                    && (trimmed.contains("fail")
                        || trimmed.contains("failure")
                        || trimmed.contains("fault")))
                || trimmed.contains("FailureHook")
                || trimmed.contains("FaultPoint");
            if !fault_name || path == store_path {
                continue;
            }
            if !line_is_cfg_test_gated(&sanitized, index) {
                violations.push(format!("{}:{}: {}", relative, index + 1, trimmed));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "session-store failure controls must remain inside #[cfg(test)] items/modules:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_receiver_aware_compatibility_absence_and_retained_api_guard() {
    let scan = SourceScan::new();
    let mut methods = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src/runtime")) {
        collect_coding_agent_session_methods(&scan.repo_root, &path, &mut methods);
    }

    let mut expected = Vec::new();
    add_expectations(
        &mut expected,
        "canonical dispatcher",
        "pub",
        false,
        &["run", "submit"],
    );
    let absent = [
        "create_extension_staging_directory",
        "install_extension_staged",
        "activate_extensions",
        "invoke_agent",
        "invoke_team",
        "export_current",
        "export_current_html",
        "set_default_agent_profile_id",
        "prompt",
        "compact",
        "self_healing_edit",
        "self_healing_edit_with_options",
        "reload_plugins",
        "run_plugin_command",
        "approve_delegation_confirmation",
        "reject_delegation_confirmation",
        "fork_current_session",
        "summarize_branch",
        "summarize_branch_for_navigation",
        "subscribe",
        "connect_client",
    ];
    add_expectations(
        &mut expected,
        "retained lifecycle/query/event/control helper",
        "pub",
        false,
        &[
            "create",
            "open",
            "open_or_create",
            "non_persistent",
            "list",
            "export_session_html",
            "subscribe_product_events_public",
            "runtime_shutdown_handle",
            "capability_control",
            "shutdown",
            "snapshot",
            "review_changed_file",
            "revalidate_external_editor_target",
            "transcript_snapshot",
            "session_storage_path",
            "connect",
            "capabilities",
            "view",
            "recovery_pending",
            "resolve_recovery",
            "retry_recovery",
            "agent_profiles",
            "team_profiles",
            "profile_diagnostics",
            "pending_delegation_confirmations",
            "pending_tool_authorizations",
            "decide_tool_authorization",
        ],
    );
    add_expectations(
        &mut expected,
        "categorized crate-internal facade bridge",
        "pub(crate)",
        false,
        &[
            "create_internal",
            "open_internal",
            "open_or_create_internal",
            "non_persistent_internal",
            "list_internal",
            "list_overviews_internal",
            "export_session_html_internal",
            "transcript_snapshot_internal",
            "shutdown_internal",
            "connect_internal",
            "resolve_recovery_internal",
            "retry_recovery_internal",
            "decide_tool_authorization_internal",
            "recovery_pending_internal",
            "run_internal",
            "submit_internal",
            "discard_submission_lease",
            "owns_submission_coordinator",
        ],
    );
    add_expectations(
        &mut expected,
        "retained lifecycle/query/event/control helper",
        "pub(crate)",
        false,
        &[
            "hydrate",
            "tree_view",
            "clone_session",
            "fork_session",
            "hydrate_current",
            "subscribe_product_events",
            "install_submission_lease",
            "resolve_recovery_with_authority",
        ],
    );
    add_expectations(
        &mut expected,
        "test-only helper",
        "pub(crate)",
        true,
        &[
            "non_persistent_with_event_capacity_for_tests",
            "non_persistent_with_event_capacities_for_tests",
            "arm_append_events_failure_for_tests",
            "prompt_control_handle",
            "arm_update_manifest_failure_for_tests",
            "queue_pending_delegation_for_tests",
            "ui_snapshot",
        ],
    );

    let mut violations = Vec::new();
    for name in absent {
        let definitions = methods.iter().filter(|method| method.name == name).count();
        if definitions != 0 {
            violations.push(format!(
                "deleted compatibility method `{name}` must have no CodingAgentSession definition, found {definitions}"
            ));
        }
    }
    violations.extend(absent_receiver_calls(&scan, &absent));
    violations.extend(local_deprecation_suppression_violations(&scan, &absent));
    for expectation in &expected {
        let definitions = methods
            .iter()
            .filter(|method| method.name == expectation.name)
            .collect::<Vec<_>>();
        if definitions.len() != 1 {
            violations.push(format!(
                "{} `{}` expected exactly once, found {}: {}",
                expectation.group,
                expectation.name,
                definitions.len(),
                format_method_locations(&definitions)
            ));
            continue;
        }
        let method = definitions[0];
        if method.visibility != expectation.visibility || method.test_only != expectation.test_only
        {
            violations.push(format!(
                "{} `{}` has visibility/test gate {}/{}, expected {}/{} at {}:{}",
                expectation.group,
                expectation.name,
                method.visibility,
                method.test_only,
                expectation.visibility,
                expectation.test_only,
                method.file,
                method.line
            ));
        }
    }
    for method in &methods {
        let groups = expected
            .iter()
            .filter(|expectation| expectation.name == method.name)
            .map(|expectation| expectation.group)
            .collect::<Vec<_>>();
        if groups.len() != 1 {
            let diagnostic_context = unexpected_method_context(method);
            violations.push(format!(
                "method `{}` belongs to {} allowed groups ({:?}) at {}:{}-{}{}",
                method.name,
                groups.len(),
                groups,
                method.file,
                method.line,
                method.end_line,
                diagnostic_context,
            ));
        }
    }
    violations.extend(alternate_facade_violations(&scan));

    let lib = sanitize_rust_source(
        &fs::read_to_string(scan.crate_root.join("src/lib.rs")).expect("read crate lib source"),
    );
    assert_eq!(
        lib.matches("pub mod api").count(),
        1,
        "lib.rs::api must remain the sole stable facade"
    );
    assert!(
        violations.is_empty(),
        "CodingAgentSession public/pub(crate) method ledger changed:\n{}",
        violations.join("\n")
    );
}

fn absent_receiver_calls(scan: &SourceScan, names: &[&str]) -> Vec<String> {
    let mut paths = rust_files_under(&scan.crate_root.join("src"));
    paths.extend(rust_files_under(&scan.crate_root.join("tests")));
    let mut violations = Vec::new();
    for path in paths {
        let relative = relative_path(&scan.repo_root, &path);
        if relative == "crates/coding-agent/tests/events_snapshot/event_boundary_guards.rs" {
            continue;
        }
        let source = sanitize_rust_source(&fs::read_to_string(&path).expect("read Rust source"));
        let lines = source.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            for name in names {
                let pattern = format!(".{name}(");
                if line.contains(&pattern) {
                    if *name == "subscribe" && line.contains("lifecycle_sender") {
                        continue;
                    }
                    if (*name == "prompt" && line.contains("agent.prompt("))
                        || (*name == "set_default_agent_profile_id"
                            && (line.contains("session_service.set_default_agent_profile_id(")
                                || line.contains("root.set_default_agent_profile_id(")
                                || line.contains("self.set_default_agent_profile_id(")))
                    {
                        continue;
                    }
                    violations.push(format!(
                        "deleted G1 receiver call `{name}` remains at {relative}:{}: {}",
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    violations
}

fn local_deprecation_suppression_violations(scan: &SourceScan, names: &[&str]) -> Vec<String> {
    let mut paths = rust_files_under(&scan.crate_root.join("src"));
    paths.extend(rust_files_under(&scan.crate_root.join("tests")));
    let mut violations = Vec::new();
    for path in paths {
        if relative_path(&scan.repo_root, &path)
            == "crates/coding-agent/tests/events_snapshot/event_boundary_guards.rs"
        {
            continue;
        }
        let source = sanitize_rust_source(&fs::read_to_string(&path).expect("read Rust source"));
        let lines = source.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("#[allow(deprecated)]") {
                continue;
            }
            let window = lines[index..usize::min(index + 12, lines.len())].join("\n");
            if names.iter().any(|name| window.contains(name)) {
                violations.push(format!(
                    "local deprecated suppression remains near deleted compatibility method at {}:{}",
                    relative_path(&scan.repo_root, &path),
                    index + 1
                ));
            }
        }
    }
    violations
}

#[test]
fn product_sources_do_not_register_global_provider_runtime_outside_compat_boundary() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    collect_source_violations(
        scan.repo_root(),
        &scan.crate_root.join("src"),
        &[],
        &mut violations,
        |line| {
            line.contains("register_builtin_providers_for_global_runtime(")
                || line.contains("ai::providers::register_builtins()")
        },
    );

    assert!(
        violations.is_empty(),
        "product source must not register the global provider runtime outside the explicit compatibility boundary; normal product execution uses scoped AiClient runtime paths:\n{}",
        violations.join("\n")
    );
}

#[test]
fn adapters_do_not_construct_or_run_low_level_agents() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for relative_root in [
        "../cli/src/interactive",
        "../cli/src/cli/headless.rs",
        "src/protocol",
        "src/app/prompt_execution.rs",
    ] {
        collect_source_violations(
            scan.repo_root(),
            &scan.crate_root.join(relative_root),
            &[],
            &mut violations,
            |line| {
                line.contains("Agent::new(")
                    || line.contains("Agent::with_messages(")
                    || line.contains("use agent_core::api::agent::Agent;")
                    || line.contains("agent_core::api::agent::{Agent,")
                    || line.contains("use agent_core::api::Agent;")
                    || line.contains("use agent_core::api::{Agent,")
                    || line.contains("use agent_core::api::{ Agent,")
            },
        );
    }

    assert!(
        violations.is_empty(),
        "adapters should route product execution through CodingAgentSession instead of low-level Agent construction or execution:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_json_and_print_use_canonical_headless_boundary() {
    let scan = SourceScan::new();
    for retired in [
        "src/adapters/json/mod.rs",
        "src/adapters/print.rs",
        "tests/print_json/json_mode.rs",
    ] {
        assert!(
            !scan.crate_root.join(retired).exists(),
            "headless presentation and superseded private-seed fixtures must remain deleted from the product crate: {retired}"
        );
    }

    let headless = fs::read_to_string(scan.crate_root.join("../cli/src/cli/headless.rs"))
        .expect("read cli headless presentation");
    for required in [
        "CodingAgentPromptExecution",
        "run_print(execution).await",
        "run_json(execution, cwd).await",
        "CodingAgentPromptExecutionUpdate::Event",
        "CodingProtocolEventAdapter",
        "headless-wire-events.json",
        "product_owned_fixture_preserves_complete_headless_jsonl_order_and_shape",
        "public_prompt_failure_keeps_stdout_valid_jsonl_and_stderr_safe",
    ] {
        assert!(
            headless.contains(required),
            "cli must own typed headless presentation: {required}"
        );
    }
    let production_headless = production_source(&sanitize_rust_source(&headless));
    for forbidden in [
        "open_headless_prompt_session",
        "PromptTurnOptions",
        "run_internal(",
        "ai::",
        "agent_core::",
    ] {
        assert!(
            !production_headless.contains(forbidden),
            "cli headless presentation must not own product runtime authority: {forbidden}"
        );
    }

    let execution = fs::read_to_string(scan.crate_root.join("src/app/prompt_execution.rs"))
        .expect("read product prompt execution source");
    assert!(
        execution.contains("open_headless_prompt_session")
            && execution.contains("PromptTurnOptions::from_prompt_runtime_options")
            && execution.contains("run_internal(CodingAgentOperation::Prompt"),
        "the product prompt execution port must retain session and operation authority"
    );
    let application = fs::read_to_string(scan.crate_root.join("src/app/application.rs"))
        .expect("read private product application resolver");
    assert!(
        application.contains("pub(crate) fn prepare_prompt_execution(")
            && application
                .contains("CodingAgentPromptExecution::from_internal(resolved.session_options)",)
            && application.contains("CodingAgentPromptExecutionPreparation::from_internal(")
            && !application.contains("adapters::print")
            && !application.contains("adapters::json")
            && !application.contains("CliOutput")
            && !application.contains("FnOnce("),
        "product invocation resolution must prepare opaque execution without owning process presentation"
    );
}

#[test]
fn production_rpc_uses_canonical_operations() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for retired in [
        "tests/events_snapshot/protocol_events.rs",
        "tests/fixtures/architecture-baseline-v1/rpc-hello-response.json",
        "tests/recovery/protocol_sessions.rs",
        "tests/rpc/mode.rs",
    ] {
        assert!(
            !scan.crate_root.join(retired).exists(),
            "superseded product-owned RPC/protocol fixture must remain deleted: {retired}"
        );
    }

    // RPC production source must submit operations through
    // CodingAgentSession::run instead of replaced broad workflow methods
    // (both deprecated and non-deprecated), and must not suppress
    // deprecation warnings in production source. Test-only allowances
    // inside #[cfg(test)] modules are preserved.
    let replaced_workflow_methods = [
        // Deprecated broad workflow methods
        "prompt",
        "compact",
        "self_healing_edit",
        "self_healing_edit_with_options",
        "invoke_agent",
        "invoke_team",
        "summarize_branch",
        "export_current",
        "export_current_html",
        // Non-deprecated methods replaced by canonical operations
        "approve_delegation_confirmation",
        "reject_delegation_confirmation",
        "set_default_agent_profile_id",
        "reload_plugins",
        "run_plugin_command",
    ];

    for path in rust_files_under(&scan.crate_root.join("../cli/src/rpc")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source =
            fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {relative}: {err}"));
        let sanitized = sanitize_rust_source(&source);
        for (index, line) in sanitized.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || line_is_cfg_test_gated(&sanitized, index) {
                continue;
            }
            if trimmed.contains("#[allow(deprecated)]") {
                violations.push(format!(
                    "{relative}:{}: production RPC source suppresses deprecation: {}",
                    index + 1,
                    trimmed
                ));
            }
            for method in replaced_workflow_methods {
                let pattern = format!(".{method}(");
                if trimmed.contains(&pattern) {
                    violations.push(format!(
                        "{relative}:{}: production RPC source calls replaced workflow method `{method}` instead of CodingAgentSession::run: {}",
                        index + 1,
                        trimmed
                    ));
                }
            }
            for forbidden in [
                "CodingAgentSession::create(",
                "CodingAgentSession::open(",
                "CodingAgentSession::open_or_create(",
                "CodingAgentSession::list(",
                "CodingAgentSession::fork_session(",
                "CodingAgentSession::non_persistent(",
                "resolve_session_dir(",
            ] {
                if trimmed.contains(forbidden) {
                    violations.push(format!(
                        "{relative}:{}: production RPC source owns session preparation instead of delegating to app/session: {}",
                        index + 1,
                        trimmed
                    ));
                }
            }
        }
    }

    let prompt_source = fs::read_to_string(scan.crate_root.join("../cli/src/rpc/prompt.rs"))
        .expect("read RPC prompt source");
    let commands_source = fs::read_to_string(scan.crate_root.join("../cli/src/rpc/commands.rs"))
        .expect("read RPC commands source");
    assert!(prompt_source.contains("application.session_bootstrap.open()"));
    assert!(!prompt_source.contains("runtime_session_root"));
    assert!(commands_source.contains("application.session_bootstrap.open()"));
    let state_source = fs::read_to_string(scan.crate_root.join("../cli/src/rpc/state.rs"))
        .expect("read RPC state source");
    let state_production = state_source
        .split("#[cfg(test)]")
        .next()
        .expect("RPC state has production source");
    assert!(state_production.contains("CodingAgentApplicationStartup"));
    assert!(!state_production.contains("CliRunOptions"));
    for forbidden in [
        "config::load_config(",
        "select_model(",
        "config::auth::resolve_api_key(",
    ] {
        assert!(
            !state_production.contains(forbidden),
            "RPC state must consume app-owned runtime defaults: {forbidden}"
        );
    }
    let stats_source = fs::read_to_string(scan.crate_root.join("../cli/src/rpc/stats.rs"))
        .expect("read RPC stats source");
    assert!(stats_source.contains("CodingAgentCapabilities::idle"));
    assert!(!stats_source.contains("crate::runtime::control"));
    assert!(!stats_source.contains("PluginCapabilities"));

    assert!(
        violations.is_empty(),
        "RPC production source must route operations through CodingAgentSession::run/submit and must not call replaced broad workflow methods or suppress deprecation:\n{}",
        violations.join("\n")
    );

    let prompt = fs::read_to_string(scan.crate_root.join("../cli/src/rpc/prompt.rs"))
        .expect("read RPC prompt owner");
    let prompt = sanitize_rust_source(&prompt);
    assert!(prompt.contains(".agent_invocation_operation("));
    assert!(prompt.contains(".team_invocation_operation("));
    assert!(prompt.contains("prepare_client_submission("));
    assert!(prompt.matches(".submit(operation)").count() >= 2);
    for path in rust_files_under(&scan.crate_root.join("src")) {
        let source = fs::read_to_string(&path).expect("read production source");
        assert!(!source.contains("PluginCommand"));
        assert!(!source.contains("plugin_command"));
    }
}

#[test]
fn production_rpc_projects_the_public_client_connection_without_authority_mirrors() {
    let scan = SourceScan::new();
    let state_path = scan.crate_root.join("../cli/src/rpc/state.rs");
    let prompt_path = scan.crate_root.join("../cli/src/rpc/prompt.rs");
    let state = fs::read_to_string(&state_path).expect("read RPC state");
    let prompt = fs::read_to_string(&prompt_path).expect("read RPC prompt");
    let state_production = state.split("#[cfg(").next().unwrap();
    let prompt_production = prompt.split("#[cfg(").next().unwrap();

    assert!(state_production.contains("client_connection"));
    assert!(state_production.contains("CodingAgentClientConnection"));
    assert!(prompt_production.contains("connection.reconnect_from_cursor("));
    assert!(prompt_production.contains("connection.acknowledge("));
    assert!(prompt_production.contains("connection.prepare_client_submission("));
    assert!(prompt_production.contains("let outcome = submission"));
    assert!(prompt_production.contains(".run(&mut session)"));
    assert!(!prompt_production.contains("set_prompt_operation_draft("));
    assert!(prompt_production.contains(".run(CodingAgentOperation::ApproveDelegation"));
    assert!(prompt_production.contains(".run(operation)"));

    for prohibited in [
        "client_drafts:",
        "submitted_operation:",
        "ProductEventReplayHandle",
        "PromptControlHandle",
        "replayed_through_sequence",
        "product_event_replay:",
    ] {
        assert!(
            !state_production.contains(prohibited) && !prompt_production.contains(prohibited),
            "RPC must not reintroduce client authority mirror `{prohibited}`"
        );
    }
}

#[test]
fn retired_interactive_private_seed_fixtures_stay_split_by_owner() {
    let scan = SourceScan::new();
    for retired in [
        "src/internal_tests/interactive_abort.rs",
        "src/internal_tests/interactive_mode.rs",
        "src/internal_tests/interactive_sessions.rs",
        "src/internal_tests/interactive_event_bridge.rs",
        "src/internal_tests/interactive_transcript.rs",
        "tests/session/cli.rs",
    ] {
        assert!(
            !scan.crate_root.join(retired).exists(),
            "superseded product interactive/private-seed fixture must remain deleted: {retired}"
        );
    }

    let facade_tests = fs::read_to_string(scan.crate_root.join("src/runtime/facade/tests.rs"))
        .expect("read typed product runtime tests");
    for required in [
        "runtime_owned_agent_invocation_abort_cancels_by_operation_identity",
        "non_persistent_constructor_does_not_create_session_files",
        "canonical_run_forks_current_session",
        "canonical_run_preserves_navigation_and_branch_summary_durability",
        "canonical_run_preserves_profile_and_delegation_contracts",
        "compact_persistent_session_records_events_and_replays_summary",
        "prompt_hydrates_replayed_transcript_when_opening_session",
    ] {
        assert!(
            facade_tests.contains(required),
            "typed product runtime evidence must retain {required}"
        );
    }

    let delegation_tests = fs::read_to_string(
        scan.crate_root
            .join("tests/operation/delegation_execution.rs"),
    )
    .expect("read typed delegation execution tests");
    assert!(
        delegation_tests
            .contains("parent_abort_drops_stalled_child_stream_and_prevents_late_continuation"),
        "typed product delegation evidence must retain parent/child abort propagation"
    );
}

#[test]
fn production_interactive_uses_canonical_operations() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    // Interactive production source must submit operations through
    // CodingAgentSession::run instead of replaced broad workflow methods
    // (both deprecated and non-deprecated), and must not suppress
    // deprecation warnings in production source. The legitimate local
    // InteractiveRoot::set_default_agent_profile_id projection setter is
    // explicitly allowed. Test-only allowances inside #[cfg(test)] modules
    // are preserved.
    let replaced_workflow_methods = [
        // Deprecated broad workflow methods
        "prompt",
        "compact",
        "self_healing_edit",
        "self_healing_edit_with_options",
        "invoke_agent",
        "invoke_team",
        "summarize_branch",
        "export_current",
        "export_current_html",
        // Non-deprecated methods replaced by canonical operations
        "approve_delegation_confirmation",
        "reject_delegation_confirmation",
        "reload_plugins",
        "run_plugin_command",
        "fork_current_session",
        "summarize_branch_for_navigation",
    ];

    for path in rust_files_under(&scan.crate_root.join("../cli/src/interactive")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source =
            fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {relative}: {err}"));
        let sanitized = sanitize_rust_source(&source);
        for (index, line) in sanitized.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || line_is_cfg_test_gated(&sanitized, index) {
                continue;
            }
            if trimmed.contains("#[allow(deprecated)]") {
                violations.push(format!(
                    "{relative}:{}: production interactive source suppresses deprecation: {}",
                    index + 1,
                    trimmed
                ));
            }
            for method in replaced_workflow_methods {
                let pattern = format!(".{method}(");
                if trimmed.contains(&pattern) {
                    violations.push(format!(
                        "{relative}:{}: production interactive source calls replaced workflow method `{method}` instead of CodingAgentSession::run: {}",
                        index + 1,
                        trimmed
                    ));
                }
            }
            // set_default_agent_profile_id is both a legitimate InteractiveRoot
            // projection setter and a replaced CodingAgentSession method. Allow
            // root.set_default_agent_profile_id( and self.set_default_agent_profile_id(
            // (the root's own internal call); reject any other receiver.
            if trimmed.contains(".set_default_agent_profile_id(")
                && !trimmed.contains("root.set_default_agent_profile_id(")
                && !trimmed.contains("self.set_default_agent_profile_id(")
            {
                violations.push(format!(
                    "{relative}:{}: production interactive source calls replaced session method `set_default_agent_profile_id` on a non-root receiver instead of CodingAgentSession::run(SetDefaultAgentProfile): {}",
                    index + 1,
                    trimmed
                ));
            }
            // Reject private runtime contract imports from the coding_session
            // module. Migrated adapters must import operation types through
            // crate::api. Check the import prefix and private type names
            // separately to avoid false matches.
            let coding_session_prefix = ["crate::coding_", "session"].concat();
            if trimmed.contains("use ")
                && trimmed.contains(&coding_session_prefix)
                && trimmed.contains("::")
            {
                for private_type in [
                    "Operation",
                    "PluginLoadOptions",
                    "OperationDescriptor",
                    "OperationExecution",
                    "WorkflowService",
                    "SessionService",
                    "EventService",
                    "CapabilityService",
                    "CapabilitySnapshotService",
                    "RuntimeService",
                    "IntentRouter",
                ] {
                    if trimmed.contains(private_type) {
                        violations.push(format!(
                            "{relative}:{}: production interactive source imports private runtime contract `{private_type}` from the coding_session module instead of crate::api: {}",
                            index + 1,
                            trimmed
                        ));
                    }
                }
            }
        }
    }

    // Verify migrated adapters import operation types through the categorized facade.
    let prompt_task_source = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/prompt_task.rs"),
    )
    .expect("read prompt_task source");
    let sanitized_prompt_task = production_source(&sanitize_rust_source(&prompt_task_source));
    assert!(
        sanitized_prompt_task.contains("use coding_agent::api::"),
        "interactive prompt_task must import CodingAgentOperation/CodingAgentOperationOutcome through coding_agent::api per D-16"
    );
    assert!(sanitized_prompt_task.contains("CodingAgentSessionBootstrap"));
    assert!(sanitized_prompt_task.contains("open_task_session("));
    assert!(sanitized_prompt_task.contains(".open()"));
    assert!(sanitized_prompt_task.contains("prepare_interactive_submission("));
    assert!(sanitized_prompt_task.contains("run_interactive_submission("));
    assert!(sanitized_prompt_task.contains("submission.run(session)"));
    for forbidden in [
        "session.run_internal(CodingAgentOperation::Prompt",
        "set_prompt_operation_draft(",
    ] {
        assert!(
            !sanitized_prompt_task.contains(forbidden),
            "interactive must not rebuild product submission choreography: {forbidden}"
        );
    }
    for forbidden in [
        "CodingAgentSession::create(",
        "CodingAgentSession::open(",
        "CodingAgentSession::open_or_create(",
        "CodingAgentSession::list(",
        "CodingAgentSession::fork_session(",
        "CodingAgentSession::non_persistent(",
        "resolve_session_dir(",
    ] {
        assert!(
            !sanitized_prompt_task.contains(forbidden),
            "interactive prompt_task must delegate session preparation to app/session: {forbidden}"
        );
    }
    let session_actions_source = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/session_actions.rs"),
    )
    .expect("read interactive session actions source");
    let session_actions_production =
        production_source(&sanitize_rust_source(&session_actions_source));
    for required in [
        "CodingAgentSessionBootstrap",
        "selected_snapshot",
        "CodingAgentSessionQuery",
        ".clone_session(",
        ".tree(",
        ".export_html(",
    ] {
        assert!(session_actions_production.contains(required));
    }
    for forbidden in [
        "CodingAgentSession::hydrate(",
        "CodingAgentSession::list(",
        "CodingAgentSession::clone_session(",
        "CodingAgentSession::tree_view(",
        "CodingAgentSession::export_session_html(",
    ] {
        assert!(
            !session_actions_production.contains(forbidden),
            "interactive session_actions must project app-owned session commands: {forbidden}"
        );
    }
    let interactive_app_source =
        fs::read_to_string(scan.crate_root.join("../cli/src/interactive/app.rs"))
            .expect("read interactive app source");
    let interactive_startup_source =
        fs::read_to_string(scan.crate_root.join("src/app/interactive.rs"))
            .expect("read product interactive startup source");
    assert!(interactive_app_source.contains("CodingAgentInteractiveStartup"));
    assert!(interactive_startup_source.contains("resolve_application_context_from_options"));
    assert!(interactive_startup_source.contains("resolve_profile_registry"));
    assert!(interactive_startup_source.contains("configured_model_choices"));
    assert!(interactive_startup_source.contains("rotation_model_choices"));
    for forbidden in [
        "config::resolve_paths(",
        "ProfileRegistry::load(",
        "discover_context_files(",
        "config::auth::resolve_api_key(",
        "parse_model_rotation(",
        "ai::api::model::all_models(",
    ] {
        assert!(
            !interactive_app_source.contains(forbidden),
            "interactive adapter must consume app-owned startup resolution: {forbidden}"
        );
    }
    let interactive_loop_source =
        fs::read_to_string(scan.crate_root.join("../cli/src/interactive/loop.rs"))
            .expect("read interactive loop source");
    assert!(!interactive_loop_source.contains("resolve_provider_api_key"));
    assert!(!interactive_loop_source.contains("config::auth::resolve_api_key("));
    assert!(interactive_loop_source.contains("apply_settings_command"));
    assert!(!interactive_loop_source.contains("persist_global_settings"));
    assert!(!interactive_loop_source.contains("merge_and_save_settings("));
    let interactive_commands_source =
        fs::read_to_string(scan.crate_root.join("../cli/src/interactive/commands.rs"))
            .expect("read interactive commands source");
    assert!(interactive_commands_source.contains("CodingAgentAuthCommand"));
    assert!(!interactive_commands_source.contains("save_provider_api_key"));
    assert!(!interactive_commands_source.contains("remove_provider_auth"));
    assert!(!interactive_commands_source.contains("AuthStore"));
    assert!(!interactive_commands_source.contains("config::resolve_paths("));
    assert!(!interactive_commands_source.contains(".auth.save("));
    assert!(!interactive_commands_source.contains("ProfileRegistry"));
    let interactive_root_source =
        fs::read_to_string(scan.crate_root.join("../cli/src/interactive/root.rs"))
            .expect("read interactive root source");
    let interactive_root_production =
        production_source(&sanitize_rust_source(&interactive_root_source));
    assert!(!interactive_root_production.contains("profile_catalog_for_cwd"));
    assert!(interactive_root_source.contains("CodingAgentAuthSnapshot"));
    assert!(interactive_root_source.contains("CodingAgentSettingsSnapshot"));
    assert!(interactive_root_source.contains("CodingAgentSettingsCommand"));
    assert!(!interactive_root_source.contains("AuthStore"));
    assert!(!interactive_root_source.contains("PartialSettings"));
    assert!(!interactive_root_source.contains("settings_delta"));
    assert!(!interactive_root_source.contains("crate::config::Settings"));
    assert!(!interactive_root_source.contains("config::resolve_paths("));
    assert!(!interactive_root_source.contains("ProfileRegistry::load("));
    assert!(!interactive_root_source.contains("ProfileRegistry"));
    assert!(!interactive_root_source.contains("DelegationTargetInventory::from_registry"));
    let interactive_profile_menu_source = fs::read_to_string(
        scan.crate_root
            .join("../cli/src/interactive/profile_menu.rs"),
    )
    .expect("read interactive profile menu source");
    assert!(interactive_profile_menu_source.contains("CodingAgentProfileCatalog"));
    assert!(!interactive_profile_menu_source.contains("ProfileRegistry"));
    assert!(!interactive_profile_menu_source.contains(".path"));
    for forbidden in [
        "pub(super) settings: crate::config::Settings",
        "profile_registry: ProfileRegistry",
        "pub(super) session_name",
        "settings: self.settings.clone()",
        "session_name: self.session_name.clone()",
    ] {
        assert!(
            !interactive_app_source.contains(forbidden),
            "interactive prompt context must not retain or return complete product settings: {forbidden}"
        );
    }
    let loop_source = fs::read_to_string(scan.crate_root.join("../cli/src/interactive/loop.rs"))
        .expect("read interactive loop source");
    let sanitized_loop = sanitize_rust_source(&loop_source);
    assert!(
        sanitized_loop.contains("use coding_agent::api::"),
        "interactive loop must import public operation projections through coding_agent::api per D-16"
    );

    assert!(
        violations.is_empty(),
        "interactive production source must route operations through CodingAgentSession::run and must not call replaced broad workflow methods, suppress deprecation, or import private runtime contracts:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_adapters_do_not_introduce_switch_active_leaf() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    // CodeGraph discovery found no first-party SwitchActiveLeaf caller. Audit
    // that no production adapter introduces one. The SwitchActiveLeaf operation
    // remains in the public enum for completeness but has no live caller.
    for relative_root in [
        "../cli/src/interactive",
        "../cli/src/cli/headless.rs",
        "src/protocol",
    ] {
        collect_source_violations(
            scan.repo_root(),
            &scan.crate_root.join(relative_root),
            &[],
            &mut violations,
            |line| {
                line.contains(".switch_active_leaf(")
                    || line.contains("CodingAgentOperation::SwitchActiveLeaf")
            },
        );
    }

    assert!(
        violations.is_empty(),
        "production adapters must not introduce a SwitchActiveLeaf caller; CodeGraph found none and the operation has no live first-party caller:\n{}",
        violations.join("\n")
    );
}

#[test]
fn adapters_do_not_access_event_service_directly_for_projection() {
    let scan = SourceScan::new();

    for relative_path in [
        "../cli/src/rpc/commands.rs",
        "../cli/src/rpc/stats.rs",
        "../cli/src/rpc/prompt.rs",
        "../cli/src/interactive/loop.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative_path))
            .unwrap_or_else(|err| panic!("read {relative_path}: {err}"));
        assert!(
            !source.contains(".event_service."),
            "{relative_path} should project through snapshot/product-event facades instead of accessing EventService directly"
        );
    }
}

#[test]
fn runtime_service_production_paths_require_capability_snapshot() {
    let scan = SourceScan::new();
    let runtime_service_source =
        fs::read_to_string(scan.crate_root.join("src/services/runtime.rs"))
            .expect("read runtime service source");

    assert_fn_is_test_gated(&runtime_service_source, "fn build_agent_runtime(");
    assert_fn_is_test_gated(
        &runtime_service_source,
        "fn build_agent_runtime_with_diagnostics(",
    );
    assert_fn_is_not_test_gated(
        &runtime_service_source,
        "fn build_agent_runtime_with_capabilities(",
    );

    let mut violations = Vec::new();
    collect_source_violations(
        scan.repo_root(),
        &scan.crate_root.join("src"),
        &["crates/coding-agent/src/services/runtime.rs"],
        &mut violations,
        |line| {
            line.contains(".build_agent_runtime_with_diagnostics(")
                || line.contains(".build_agent_runtime(")
        },
    );

    assert!(
        violations.is_empty(),
        "production runtime build must route through build_agent_runtime_with_capabilities; permissive compat wrappers must not be called outside runtime_service tests:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_child_operations_have_no_permissive_capability_fallback() {
    let scan = SourceScan::new();
    let capability = fs::read_to_string(scan.crate_root.join("src/runtime/capability.rs"))
        .expect("read capability source");
    let agent = fs::read_to_string(
        scan.crate_root
            .join("src/operations/agent_invocation/runner.rs"),
    )
    .expect("read agent invocation runner");
    let team = fs::read_to_string(
        scan.crate_root
            .join("src/operations/team_invocation/runner.rs"),
    )
    .expect("read team invocation runner");

    assert_fn_is_test_gated(&capability, "fn permissive(");
    for (owner, source) in [("agent", agent), ("team", team)] {
        assert!(
            !source.contains("OperationCapabilitySnapshot::permissive"),
            "{owner} child runner must fail closed when parent capabilities are absent"
        );
        assert!(
            source.contains("requires an admitted parent capability snapshot"),
            "{owner} child runner must return an explicit missing-capability error"
        );
    }
}

#[test]
fn builtin_filesystem_and_shell_tools_are_bound_from_frozen_handles() {
    let scan = SourceScan::new();
    let runtime_service = fs::read_to_string(scan.crate_root.join("src/services/runtime.rs"))
        .expect("read runtime service source");
    let tools = fs::read_to_string(scan.crate_root.join("src/tools/mod.rs"))
        .expect("read built-in tool registry source");

    assert!(
        runtime_service.contains("bind_builtin_tool_to_capabilities("),
        "RuntimeService must bind reserved built-in tools from the admitted capability snapshot"
    );
    assert!(runtime_service.contains("snapshot.filesystem.as_ref()"));
    assert!(runtime_service.contains("snapshot.shell.as_ref()"));

    for (name, constructor) in [
        ("read", "filesystem::read::read_tool"),
        ("write", "filesystem::write::write_tool"),
        ("edit", "filesystem::edit::edit_tool"),
        ("grep", "filesystem::grep::grep_tool"),
        ("find", "filesystem::find::find_tool"),
        ("ls", "filesystem::ls::ls_tool"),
        ("bash", "shell::bash_tool"),
    ] {
        assert!(
            tools.contains(&format!("\"{name}\" =>")),
            "reserved built-in tool `{name}` must have an explicit frozen-handle binding"
        );
        assert!(
            tools.contains(constructor),
            "reserved built-in tool `{name}` must be reconstructed by `{constructor}`"
        );
    }
}

#[test]
fn model_provider_paths_require_the_frozen_model_handle() {
    let scan = SourceScan::new();
    let owners = [
        "src/services/runtime.rs",
        "src/operations/compaction/runner.rs",
        "src/operations/branch_summary/runner.rs",
        "src/operations/self_healing_edit/mod.rs",
    ];

    for relative in owners {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        assert!(
            source.contains("ModelCapability::require("),
            "{relative} must authorize model/provider access with the frozen model handle"
        );
    }

    let runtime = fs::read_to_string(scan.crate_root.join("src/services/runtime.rs"))
        .expect("read runtime service source");
    assert!(runtime.contains("model_capability: &ModelCapability"));
    assert!(!runtime.contains("scoped_provider_streamer_for_runtime(runtime);"));
}

#[test]
fn product_events_use_operation_bound_capability_generation() {
    let scan = SourceScan::new();
    let intent = fs::read_to_string(scan.crate_root.join("src/runtime/intent.rs"))
        .expect("read operation permit source");
    let control = fs::read_to_string(scan.crate_root.join("src/runtime/control.rs"))
        .expect("read operation control source");
    let event = fs::read_to_string(scan.crate_root.join("src/services/event.rs"))
        .expect("read event service source");
    let recovery = fs::read_to_string(scan.crate_root.join("src/session/service.rs"))
        .expect("read startup recovery source");
    let publish_start = event
        .find("fn publish(")
        .expect("find canonical ProductEvent publish function");
    let publish_end = event[publish_start..]
        .find("#[cfg(test)]")
        .map(|offset| publish_start + offset)
        .expect("find end of ProductEvent publish function");
    let publish = &event[publish_start..publish_end];

    assert_eq!(
        intent
            .matches("bind_capability_generation(execution.capability_generation)")
            .count(),
        2,
        "root and child permits must bind their permit-owned execution generation"
    );
    assert!(control.contains("register_operation_event_context("));
    assert!(control.contains("clear_operation_event_context_if("));
    assert!(publish.contains("operation_event_contexts"));
    assert!(!control.contains("register_operation_capability_generation("));
    assert!(!publish.contains("operation_capability_generations"));
    assert!(
        !publish.contains("state.capability_generation"),
        "ProductEvent generation must not use the coordinator's current generation"
    );
    assert!(recovery.contains("runtime_generation.capability_generation"));
    assert!(event.contains("struct ProductEventEmissionContext"));
    assert!(event.contains("publish_recovery_event("));
    assert!(event.contains("capability_generation: capability_generation"));
    assert!(!event.contains("emit_with_capability_generation("));
}

#[test]
fn agent_tools_receive_the_admitted_operation_scope() {
    let scan = SourceScan::new();
    let runtime = fs::read_to_string(scan.crate_root.join("src/services/runtime.rs"))
        .expect("read runtime service source");
    let shell = fs::read_to_string(scan.crate_root.join("src/tools/shell.rs"))
        .expect("read shell tool source");

    assert!(runtime.contains("config.tool_execution_scope = Some(snapshot.operation_id.clone())"));
    assert!(shell.contains("context.cancel_token().clone()"));
    assert!(shell.contains("cancel_token.cancelled()"));
}

#[test]
fn capability_revocation_is_generation_scoped_and_closes_stale_admission() {
    let scan = SourceScan::new();
    let scheduler = fs::read_to_string(scan.crate_root.join("src/runtime/scheduler.rs"))
        .expect("read scheduler source");
    let control = fs::read_to_string(scan.crate_root.join("src/runtime/control.rs"))
        .expect("read operation control source");
    let projection = fs::read_to_string(scan.crate_root.join("src/runtime/client/projection.rs"))
        .expect("read capability control source");
    let prompt = fs::read_to_string(scan.crate_root.join("src/operations/prompt/mod.rs"))
        .expect("read prompt operation source");

    assert!(scheduler.contains("begin_root_with_capability_generation("));
    assert!(scheduler.contains("begin_child_with_capability_generation("));
    assert!(
        control.contains("generation < self.snapshot_coordinator.current_capability_generation()")
    );
    assert!(control.contains("cancel_capability_generations_before("));
    assert!(projection.contains("pub fn revoke_older_operations("));
    assert!(projection.contains("RequestCancelOlderOperations"));
    assert!(projection.contains("cancellation_requested_operation_ids"));
    assert!(prompt.contains("context.set_operation_cancellation(cancellation)"));
}

#[test]
fn session_mutating_operation_owners_require_frozen_write_capability() {
    let scan = SourceScan::new();
    let owners = [
        ("src/operations/prompt/mod.rs", 1usize),
        ("src/operations/compaction/mod.rs", 1),
        ("src/operations/branch_summary/mod.rs", 1),
        ("src/operations/self_healing_edit/mod.rs", 1),
        ("src/operations/delegation/execution.rs", 1),
        ("src/runtime/dispatch.rs", 6),
    ];

    for (relative, expected) in owners {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        assert_eq!(
            source.matches("SessionWriteCapability::require(").count(),
            expected,
            "{relative} must guard each session-mutating operation entry with the frozen write capability"
        );
    }
}

fn assert_fn_is_test_gated(source: &str, signature: &str) {
    let preceding = preceding_non_blank_line(source, signature)
        .unwrap_or_else(|| panic!("signature not found: {signature}"));
    assert!(
        preceding.trim() == "#[cfg(test)]",
        "compat fn `{signature}` must be gated behind #[cfg(test)] so production paths use build_agent_runtime_with_capabilities; preceding line: {preceding:?}"
    );
}

fn assert_fn_is_not_test_gated(source: &str, signature: &str) {
    let preceding = preceding_non_blank_line(source, signature)
        .unwrap_or_else(|| panic!("signature not found: {signature}"));
    assert!(
        preceding.trim() != "#[cfg(test)]",
        "production fn `{signature}` must not be gated behind #[cfg(test)]"
    );
}

fn preceding_non_blank_line<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let lines: Vec<&str> = source.lines().collect();
    let idx = lines.iter().position(|line| line.contains(signature))?;
    if idx == 0 {
        return Some("");
    }
    let mut i = idx - 1;
    while i > 0 && lines[i].trim().is_empty() {
        i -= 1;
    }
    Some(lines[i])
}

struct SourceScan {
    crate_root: PathBuf,
    repo_root: PathBuf,
}

impl SourceScan {
    fn new() -> Self {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = crate_root
            .parent()
            .and_then(Path::parent)
            .expect("crate should live under crates/coding-agent")
            .to_path_buf();
        Self {
            crate_root,
            repo_root,
        }
    }

    fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

fn collect_source_violations(
    repo_root: &Path,
    path: &Path,
    allowed_files: &[&str],
    violations: &mut Vec<String>,
    is_violation: impl Copy + Fn(&str) -> bool,
) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read source directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read source entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_source_violations(
                repo_root,
                &entry.path(),
                allowed_files,
                violations,
                is_violation,
            );
        }
        return;
    }

    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let relative = path
        .strip_prefix(repo_root)
        .expect("scanned file should be under repo root")
        .to_string_lossy()
        .replace('\\', "/");
    if allowed_files.contains(&relative.as_str()) {
        return;
    }

    let content = fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        if is_violation(line) {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}

fn add_expectations(
    target: &mut Vec<MethodExpectation>,
    group: &'static str,
    visibility: &'static str,
    test_only: bool,
    names: &[&'static str],
) {
    target.extend(names.iter().map(|name| MethodExpectation {
        name,
        group,
        visibility,
        test_only,
    }));
}

fn format_method_locations(methods: &[&SessionMethod]) -> String {
    methods
        .iter()
        .map(|method| format!("{}:{}-{}", method.file, method.line, method.end_line))
        .collect::<Vec<_>>()
        .join(", ")
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let Ok(metadata) = fs::metadata(root) else {
        return Vec::new();
    };
    if metadata.is_file() {
        return (root.extension().and_then(|extension| extension.to_str()) == Some("rs"))
            .then(|| root.to_path_buf())
            .into_iter()
            .collect();
    }
    if root.file_name().and_then(|name| name.to_str()) == Some("internal_tests") {
        return Vec::new();
    }
    let mut files = fs::read_dir(root)
        .expect("read Rust source directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("read Rust source entries")
        .into_iter()
        .flat_map(|entry| rust_files_under(&entry.path()))
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn relative_path(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .expect("source should be below repository root")
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_coding_agent_session_methods(
    repo_root: &Path,
    path: &Path,
    methods: &mut Vec<SessionMethod>,
) {
    let source = fs::read_to_string(path).expect("read CodingAgentSession source");
    let sanitized = sanitize_rust_source(&source);
    let relative = relative_path(repo_root, path);
    let lines = sanitized.lines().collect::<Vec<_>>();
    let mut in_impl = false;
    let mut depth = 0isize;
    let mut attributes = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !in_impl {
            let impl_suffix = trimmed.strip_prefix("impl CodingAgentSession");
            let starts_impl = impl_suffix.is_some_and(|suffix| {
                let suffix = suffix.trim_start();
                suffix.is_empty() || suffix.starts_with('{')
            });
            let opens_here = impl_suffix.is_some_and(|suffix| suffix.trim_start().starts_with('{'));
            let opens_next = starts_impl
                && !opens_here
                && lines
                    .get(index + 1..)
                    .into_iter()
                    .flatten()
                    .find(|next| !next.trim().is_empty())
                    .is_some_and(|next| next.trim().starts_with('{'));
            if opens_here || opens_next {
                in_impl = true;
                depth = brace_delta(line);
            }
            continue;
        }

        if depth == 1 {
            if trimmed.starts_with("#[") {
                attributes.push(trimmed.to_owned());
            } else if (trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate) "))
                && let Some((visibility, name)) = parse_visible_method_signature(&lines, index)
            {
                let end_index = visible_method_end(&lines, index);
                methods.push(SessionMethod {
                    name,
                    visibility,
                    test_only: attributes
                        .iter()
                        .any(|attribute| attribute == "#[cfg(test)]"),
                    attributes: attributes.clone(),
                    body: lines[index..=end_index].join("\n"),
                    file: relative.clone(),
                    line: index + 1,
                    end_line: end_index + 1,
                });
                attributes.clear();
            } else if !trimmed.is_empty() {
                attributes.clear();
            }
        }
        depth += brace_delta(line);
        if depth == 0 {
            in_impl = false;
            attributes.clear();
        }
    }
}

fn parse_visible_method_signature(lines: &[&str], start: usize) -> Option<(&'static str, String)> {
    let mut signature = String::new();
    for line in lines.iter().skip(start).take(12) {
        if !signature.is_empty() {
            signature.push(' ');
        }
        signature.push_str(line.trim());
        if signature.contains('{') {
            break;
        }
    }
    parse_visible_method(&signature)
}

fn visible_method_end(lines: &[&str], start: usize) -> usize {
    let mut saw_body = false;
    let mut depth = 0isize;
    for (index, line) in lines.iter().enumerate().skip(start) {
        let delta = brace_delta(line);
        if line.contains('{') {
            saw_body = true;
        }
        depth += delta;
        if saw_body && depth == 0 {
            return index;
        }
    }
    panic!(
        "visible method starting at line {} has no complete body",
        start + 1
    );
}

fn unexpected_method_context(method: &SessionMethod) -> String {
    let operation_vocabulary = [
        "CodingAgentOperation",
        "Operation::",
        "PromptTurnOptions",
        "AgentInvocationOptions",
        "AgentTeamOptions",
        "SelfHealingEditRequest",
    ]
    .iter()
    .filter(|token| method.body.contains(*token))
    .copied()
    .collect::<Vec<_>>();
    let forwards_to_run = method.body.contains(".run(") || method.body.contains("Self::run(");
    format!(
        "; targeted context: attributes={:?}, operation_vocabulary={operation_vocabulary:?}, forwards_to_run={forwards_to_run}",
        method.attributes
    )
}

fn alternate_facade_violations(scan: &SourceScan) -> Vec<String> {
    let mut paths = rust_files_under(&scan.crate_root.join("src/runtime"));
    paths.push(scan.crate_root.join("src/lib.rs"));
    let mut violations = Vec::new();
    for path in paths {
        let relative = relative_path(&scan.repo_root, &path);
        let source = sanitize_rust_source(&fs::read_to_string(&path).expect("read facade source"));
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            let public_trait =
                trimmed.starts_with("pub trait ") || trimmed.starts_with("pub(crate) trait ");
            if public_trait
                && (source.contains("CodingAgentSession")
                    || source.contains("CodingAgentOperation"))
                && trimmed.contains("run")
            {
                violations.push(format!(
                    "alternate public trait operation facade at {relative}:{}: {trimmed}",
                    index + 1
                ));
            }
            if trimmed.starts_with("pub use ")
                && trimmed.contains(" as ")
                && [
                    "CodingAgentSession",
                    "CodingAgentOperation",
                    "CodingAgentOperationOutcome",
                    "run",
                ]
                .iter()
                .any(|token| trimmed.contains(token))
            {
                violations.push(format!(
                    "alternate public operation alias at {relative}:{}: {trimmed}",
                    index + 1
                ));
            }
            if let Some(module_name) = public_module_name(trimmed)
                && ["facade", "compat", "workflow"]
                    .iter()
                    .any(|token| module_name.contains(token))
            {
                if relative.ends_with("src/runtime/mod.rs") && module_name == "facade" {
                    continue;
                }
                violations.push(format!(
                    "alternate public operation module `{module_name}` at {relative}:{}",
                    index + 1
                ));
            }
        }
    }
    violations
}

fn public_module_name(line: &str) -> Option<&str> {
    let suffix = line
        .strip_prefix("pub mod ")
        .or_else(|| line.strip_prefix("pub(crate) mod "))?;
    let name = suffix
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .count();
    (name > 0).then_some(&suffix[..name])
}

fn parse_visible_method(line: &str) -> Option<(&'static str, String)> {
    let visibility = if line.starts_with("pub(crate) ") {
        "pub(crate)"
    } else if line.starts_with("pub ") {
        "pub"
    } else {
        return None;
    };
    let fn_index = line.find("fn ")? + 3;
    let name = line[fn_index..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect::<String>();
    (!name.is_empty()).then_some((visibility, name))
}

fn brace_delta(line: &str) -> isize {
    line.chars().fold(0, |depth, character| match character {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn assert_direct_cfg_test(source: &str, signature: &str) {
    let lines = source.lines().collect::<Vec<_>>();
    let index = lines
        .iter()
        .position(|line| line.contains(signature))
        .unwrap_or_else(|| panic!("signature not found: {signature}"));
    let mut cursor = index;
    let mut attributes = Vec::new();
    while cursor > 0 {
        cursor -= 1;
        let trimmed = lines[cursor].trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("#[") {
            attributes.push(trimmed);
            continue;
        }
        break;
    }
    assert!(
        attributes.contains(&"#[cfg(test)]"),
        "`{signature}` must be directly gated by #[cfg(test)]; attributes: {attributes:?}"
    );
}

fn line_is_cfg_test_gated(source: &str, line_index: usize) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let mut previous = line_index;
    while previous > 0 {
        previous -= 1;
        let trimmed = lines[previous].trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "#[cfg(test)]" | "#[cfg(any())]") {
            return true;
        }
        if !trimmed.starts_with("#[") && !trimmed.starts_with("use ") {
            break;
        }
    }

    let mut depth = 0isize;
    let mut test_item_depths = Vec::new();
    let mut pending_test_cfg = false;
    for (index, line) in lines.iter().enumerate().take(line_index + 1) {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            pending_test_cfg = true;
        } else if pending_test_cfg && trimmed.contains('{') {
            test_item_depths.push(depth + 1);
            pending_test_cfg = false;
        } else if pending_test_cfg && trimmed.ends_with(';') {
            if index == line_index {
                return true;
            }
            pending_test_cfg = false;
        }
        depth += brace_delta(line);
        test_item_depths.retain(|item_depth| depth >= *item_depth);
        if index == line_index && (!test_item_depths.is_empty() || pending_test_cfg) {
            return true;
        }
    }
    false
}

fn sanitize_rust_source(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment(usize),
        String,
        Char,
        RawString(usize),
    }

    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut state = State::Code;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match state {
            State::Code if byte == b'/' && next == Some(b'/') => {
                output.push_str("  ");
                index += 2;
                state = State::LineComment;
            }
            State::Code if byte == b'/' && next == Some(b'*') => {
                output.push_str("  ");
                index += 2;
                state = State::BlockComment(1);
            }
            State::Code if byte == b'"' => {
                output.push(' ');
                index += 1;
                state = State::String;
            }
            State::Code if byte == b'\'' => {
                output.push(' ');
                index += 1;
                state = State::Char;
            }
            State::Code if byte == b'r' => {
                let mut cursor = index + 1;
                while bytes.get(cursor) == Some(&b'#') {
                    cursor += 1;
                }
                if bytes.get(cursor) == Some(&b'"') {
                    let hashes = cursor - index - 1;
                    output.extend(std::iter::repeat_n(' ', cursor - index + 1));
                    index = cursor + 1;
                    state = State::RawString(hashes);
                } else {
                    output.push(byte as char);
                    index += 1;
                }
            }
            State::Code => {
                output.push(byte as char);
                index += 1;
            }
            State::LineComment => {
                if byte == b'\n' {
                    output.push('\n');
                    state = State::Code;
                } else {
                    output.push(' ');
                }
                index += 1;
            }
            State::BlockComment(depth) if byte == b'/' && next == Some(b'*') => {
                output.push_str("  ");
                index += 2;
                state = State::BlockComment(depth + 1);
            }
            State::BlockComment(depth) if byte == b'*' && next == Some(b'/') => {
                output.push_str("  ");
                index += 2;
                state = if depth == 1 {
                    State::Code
                } else {
                    State::BlockComment(depth - 1)
                };
            }
            State::BlockComment(depth) => {
                output.push(if byte == b'\n' { '\n' } else { ' ' });
                index += 1;
                state = State::BlockComment(depth);
            }
            State::String | State::Char => {
                let quote = matches!(state, State::String)
                    .then_some(b'"')
                    .unwrap_or(b'\'');
                if byte == b'\\' {
                    output.push(' ');
                    if index + 1 < bytes.len() {
                        output.push(if bytes[index + 1] == b'\n' { '\n' } else { ' ' });
                    }
                    index += 2;
                } else {
                    output.push(if byte == b'\n' { '\n' } else { ' ' });
                    index += 1;
                    if byte == quote {
                        state = State::Code;
                    }
                }
            }
            State::RawString(hashes) => {
                if byte == b'"'
                    && bytes.get(index + 1..index + 1 + hashes)
                        == Some(vec![b'#'; hashes].as_slice())
                {
                    output.extend(std::iter::repeat_n(' ', hashes + 1));
                    index += hashes + 1;
                    state = State::Code;
                } else {
                    output.push(if byte == b'\n' { '\n' } else { ' ' });
                    index += 1;
                }
            }
        }
    }
    output
}

fn workspace_path(relative: &str) -> PathBuf {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/coding-agent")
        .to_path_buf();
    repo_root.join(relative)
}

#[test]
fn rpc_running_product_events_do_not_use_unbounded_channels() {
    let prompt_rs = fs::read_to_string(workspace_path("crates/cli/src/rpc/prompt.rs"))
        .expect("read rpc prompt source");
    let state_rs = fs::read_to_string(workspace_path("crates/cli/src/rpc/state.rs"))
        .expect("read rpc state source");

    assert!(
        !prompt_rs.contains("UnboundedSender<ProductEvent")
            && !state_rs.contains("UnboundedSender<ProductEvent")
            && !state_rs.contains("unbounded_channel::<ProductEvent"),
        "RPC ProductEvent forwarding must not use unbounded channels"
    );
    assert!(
        state_rs.contains("RpcProductEventQueue::new()")
            && state_rs.contains("session_events: Option<RpcProductEventReceiver>"),
        "RPC session event pump should route through one bounded RpcProductEventQueue"
    );
    assert!(
        state_rs.contains("background_completion_tx: mpsc::Sender<RpcBackgroundCompletion>")
            && state_rs.contains("mpsc::channel(RPC_BACKGROUND_OPERATION_LIMIT)")
    );
    assert!(
        prompt_rs.contains("RpcQueuedProductEvent::Overflow"),
        "RPC completion drains must handle queued overflow recovery items"
    );
}

#[test]
fn theme_reload_delivery_is_bounded() {
    let reload = fs::read_to_string(workspace_path("crates/coding-agent/src/theme/reload.rs"))
        .expect("read theme reload source");

    assert!(!reload.contains("mpsc::unbounded_channel"));
    assert!(reload.contains("mpsc::channel(1)"));
    assert!(reload.contains("blocking_send(ThemeReloadSignal"));
}

#[test]
fn event_receiver_lag_maps_to_snapshot_recovery_error() {
    let event_service_rs =
        fs::read_to_string(workspace_path("crates/coding-agent/src/services/event.rs"))
            .expect("read event service source");

    assert!(
        event_service_rs.contains("CodingSessionError::EventStreamLag"),
        "broadcast lag must map to event_stream_lag so clients know to request a fresh snapshot"
    );
    assert!(
        !event_service_rs.contains("event receiver lagged by {skipped} events"),
        "lag should not remain a generic resource error"
    );
}

#[test]
fn lifecycle_and_operation_control_authority_remain_narrow_and_identity_scoped() {
    let scan = SourceScan::new();
    let projection = sanitize_rust_source(
        &fs::read_to_string(scan.crate_root.join("src/runtime/client/projection.rs"))
            .expect("read public projection"),
    );
    let shutdown_handle = projection
        .split("pub struct CodingAgentRuntimeShutdownHandle")
        .nth(1)
        .expect("shutdown request handle exists")
        .split("pub struct CodingAgentCapabilityControl")
        .next()
        .expect("durability projection follows shutdown handle");
    assert_eq!(shutdown_handle.matches("pub fn ").count(), 1);
    assert!(shutdown_handle.contains("pub fn request_shutdown(&self)"));
    assert!(shutdown_handle.contains("self.coordinator.request_shutdown();"));
    for forbidden in [
        "pub coordinator",
        "client_id",
        "generation",
        "finish_shutdown",
        "wait_for_active_operation",
        "event_service",
        "emit(",
        "connect",
        "detach",
    ] {
        assert!(
            !shutdown_handle.contains(forbidden),
            "Phase A shutdown handle leaked `{forbidden}` authority"
        );
    }

    let capability_control = projection
        .split("pub struct CodingAgentCapabilityControl")
        .nth(1)
        .expect("capability revocation control exists")
        .split("pub enum CodingAgentSubmittedEventDurability")
        .next()
        .expect("durability projection follows capability control");
    assert_eq!(capability_control.matches("pub fn ").count(), 1);
    assert!(capability_control.contains("pub fn revoke_older_operations("));
    assert!(capability_control.contains("RequestCancelOlderOperations"));
    for forbidden in [
        "pub coordinator",
        "pub operation_control",
        "pub event_service",
    ] {
        assert!(
            !capability_control.contains(forbidden),
            "capability control leaked `{forbidden}` authority"
        );
    }

    let operation_control = projection
        .split("pub struct CodingAgentOperationControl")
        .nth(1)
        .expect("operation control exists")
        .split("pub struct CodingAgentPromptControl")
        .next()
        .expect("prompt control follows operation control");
    assert_eq!(operation_control.matches("pub fn ").count(), 1);
    assert!(operation_control.contains("pub fn abort("));
    for forbidden in [
        "pub coordinator",
        "pub fn steer(",
        "pub fn follow_up(",
        "CancellationToken",
    ] {
        assert!(
            !operation_control.contains(forbidden),
            "operation control leaked `{forbidden}` authority"
        );
    }

    let control_path = scan.crate_root.join("src/runtime/control.rs");
    let control = fs::read_to_string(&control_path).expect("read operation control source");
    assert!(control.contains("struct ActiveOperationIdentity"));
    assert!(control.contains("operation_id: String"));
    assert!(control.contains("active.operation_id == self.operation_id"));
    assert!(!control.contains("CompactCancellationHandle"));
    assert!(!control.contains("CompactCancellationRejection"));

    let snapshot = fs::read_to_string(scan.crate_root.join("src/runtime/snapshot.rs"))
        .expect("read snapshot coordinator");
    for required in [
        "struct OperationCancellationBinding",
        "owner: ClientHandle",
        "operation_cancellations: Mutex<HashMap<String, OperationCancellationBinding>>",
        "cancellation: OperationCancellationHandle",
        "cancellation_bindings.get(operation_id)",
        "active.owner.id != handle.id",
        "active.cancellation.request()",
        "clear_operation_cancellation_if",
    ] {
        assert!(
            snapshot.contains(required),
            "operation cancellation omitted `{required}`"
        );
    }
    assert!(
        !snapshot.contains("cancellation: CancellationToken"),
        "snapshot coordinator must route through owner-side cancellation authority"
    );

    let intent_router = sanitize_rust_source(
        &fs::read_to_string(scan.crate_root.join("src/runtime/intent.rs"))
            .expect("read intent router"),
    );
    assert_eq!(intent_router.matches("enum ControlIntent").count(), 0);
    assert_eq!(intent_router.matches("PromptControl,").count(), 0);
    assert!(!intent_router.contains("CompactControl"));

    assert!(!snapshot.contains("pub fn bind_operation_cancellation"));
    assert!(!snapshot.contains("pub fn clear_operation_cancellation_if"));
}

#[derive(Debug, Clone, Copy)]
struct AdapterClassification {
    path: &'static str,
    rationale: &'static str,
}

const ADAPTER_CLASSIFICATIONS: &[AdapterClassification] = &[
    AdapterClassification {
        path: "crates/coding-agent/src/runtime/facade.rs",
        rationale: "canonical runtime owner and sole ordinary dispatcher",
    },
    AdapterClassification {
        path: "crates/coding-agent/src/runtime/facade/connection.rs",
        rationale: "session-owned connection, snapshot, replay, and lifecycle facade",
    },
    AdapterClassification {
        path: "crates/coding-agent/src/runtime/client/projection.rs",
        rationale: "stable state/replay/control contract implementation",
    },
    AdapterClassification {
        path: "crates/coding-agent/src/runtime/execution.rs",
        rationale: "runtime-owned operation task and scoped control-owner binding",
    },
    AdapterClassification {
        path: "crates/coding-agent/src/runtime/submission.rs",
        rationale: "canonical prepared-submission admission and cleanup owner",
    },
    AdapterClassification {
        path: "crates/coding-agent/src/lib.rs",
        rationale: "stable categorized facade only",
    },
];

const PROHIBITED_SESSION_METHODS: &[&str] = &[
    "invoke_agent",
    "invoke_team",
    "export_current",
    "export_current_html",
    "prompt",
    "compact",
    "self_healing_edit",
    "reload_plugins",
    "run_plugin_command",
    "approve_delegation_confirmation",
    "reject_delegation_confirmation",
    "fork_current_session",
    "summarize_branch",
    "summarize_branch_for_navigation",
];

#[test]
fn adapter_inventory_is_recursive_and_receiver_aware() {
    let scan = SourceScan::new();
    let discovered = discover_adapter_candidates(&scan);
    let classification_violations =
        validate_adapter_classifications(&discovered, ADAPTER_CLASSIFICATIONS);
    assert!(
        classification_violations.is_empty(),
        "adapter discovery/classification ledger drifted:\n{}",
        classification_violations.join("\n")
    );
    assert!(
        !discovered
            .iter()
            .any(|path| path.starts_with("crates/cli/src/interactive/")),
        "product adapter inventory must not claim cli-owned sources"
    );
    assert!(
        !discovered
            .iter()
            .any(|path| path.starts_with("crates/coding-agent/src/adapters/")),
        "product inventory must not retain application-owned adapter sources"
    );

    for classification in ADAPTER_CLASSIFICATIONS {
        let relative = classification.path;
        let path = scan.repo_root.join(relative);
        let raw = fs::read_to_string(&path).expect("read adapter source");
        let sanitized = sanitize_rust_source(&raw);
        let production = production_source(&sanitized);
        for (line_no, line) in production.lines().enumerate() {
            for method in PROHIBITED_SESSION_METHODS {
                let needle = format!(".{method}(");
                if line.contains(&needle) {
                    panic!(
                        "prohibited workflow call `{method}` in adapter at {relative}:{}: {}",
                        line_no + 1,
                        line.trim()
                    );
                }
            }
        }
    }
}

#[test]
fn runtime_admission_has_no_direct_operation_control_bypass() {
    let scan = SourceScan::new();
    let session_path = scan.crate_root.join("src/runtime/facade.rs");
    let session_source = fs::read_to_string(&session_path).expect("read coding session source");
    let dispatch_path = scan.crate_root.join("src/runtime/dispatch.rs");
    let dispatch_source =
        fs::read_to_string(&dispatch_path).expect("read operation dispatch source");
    let session_production = production_source(&sanitize_rust_source(&session_source));
    let dispatch_production = production_source(&sanitize_rust_source(&dispatch_source));
    let scheduler_admission_count = session_production
        .matches("OperationScheduler::admit(")
        .count()
        + dispatch_production
            .matches("OperationScheduler::admit(")
            .count();
    assert_eq!(
        scheduler_admission_count, 3,
        "canonical sync/async dispatchers must all route through typed scheduler admission"
    );
    assert!(
        !session_production.contains("IntentRouter::admit_operation")
            && !session_production.contains("IntentRouter::begin"),
        "legacy router-owned admission entry points must not return to production dispatch"
    );

    let mut violations = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source = fs::read_to_string(&path).expect("read product source");
        let production = production_source(&sanitize_rust_source(&source));
        for (line_no, line) in production.lines().enumerate() {
            let bypass = line.contains("control.begin(")
                || line.contains("operation_control.begin(")
                || line.contains("state.begin(")
                || line.contains(".begin(OperationKind::");
            if !bypass {
                continue;
            }
            let owner = relative.ends_with("src/runtime/scheduler.rs")
                || relative.ends_with("src/runtime/control.rs");
            if !owner {
                violations.push(format!("{}:{}: {}", relative, line_no + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "runtime-affecting product code bypasses OperationScheduler admission:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_runtime_has_no_permanently_disabled_fallbacks() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for path in rust_files_under(&scan.crate_root.join("src")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source = fs::read_to_string(&path).expect("read product source");
        let production = production_source(&sanitize_rust_source(&source));
        for (line_no, line) in production.lines().enumerate() {
            if line.contains("cfg(any())") || line.contains("cfg(all(any()") {
                violations.push(format!("{}:{}: {}", relative, line_no + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "production runtime contains permanently disabled fallback paths:\n{}",
        violations.join("\n")
    );
}

#[test]
fn durable_operation_paths_consume_admitted_identity_without_regeneration() {
    let scan = SourceScan::new();
    let transaction = fs::read_to_string(scan.crate_root.join("src/session/transaction.rs"))
        .expect("read transaction source");
    assert!(
        transaction.contains("begin_admitted_with_runtime_generation"),
        "production transaction construction must expose the admitted-identity entry point"
    );
    assert!(
        transaction.matches("next_root_operation_id()").count() == 1,
        "only the test-only transaction compatibility constructor may mint an identity"
    );
    assert_direct_cfg_test(&transaction, "pub(crate) fn begin_with_runtime_generation(");

    for relative in [
        "src/operations/agent_invocation/runner.rs",
        "src/operations/team_invocation/runner.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read invocation flow source");
        let production = source;
        assert!(
            !production.contains("with_scheduler_parent_operation_id"),
            "invocation contexts must receive their execution identity at construction: {relative}"
        );
        assert!(
            !production.contains("next_root_operation_id()")
                && !production.contains("next_child_operation_id()"),
            "invocation flows must request child identities from the scheduler: {relative}"
        );
        assert!(
            production.contains("OperationScheduler::allocate_child_operation_id()"),
            "invocation flows must allocate child identities through the scheduler: {relative}"
        );
    }

    let scheduler = fs::read_to_string(scan.crate_root.join("src/runtime/scheduler.rs"))
        .expect("read scheduler source");
    assert!(
        scheduler.contains("pub(crate) fn allocate_child_operation_id()"),
        "the scheduler must own child operation identity allocation"
    );

    let delegation = fs::read_to_string(
        scan.crate_root
            .join("src/operations/delegation/execution.rs"),
    )
    .expect("read delegation execution source");
    assert!(
        delegation.contains(
            "let approval_operation_id = parent_capability_snapshot.operation_id.clone();"
        ),
        "delegation approval persistence must reuse the admitted approval identity"
    );
    assert!(
        !delegation.contains("next_root_operation_id()")
            && !delegation.contains("next_child_operation_id()"),
        "delegation execution must not mint an unadmitted approval identity"
    );

    let dispatch = fs::read_to_string(scan.crate_root.join("src/runtime/dispatch.rs"))
        .expect("read operation dispatch source");
    let execution = fs::read_to_string(scan.crate_root.join("src/runtime/execution.rs"))
        .expect("read runtime-owned execution source");
    assert_eq!(
        dispatch
            .matches("commit_execution(operation_permit.execution())")
            .count()
            + execution
                .matches("commit_execution(operation_permit.execution())")
                .count(),
        4,
        "every dispatcher must hand the admitted execution to submission finalization"
    );
    let submission = fs::read_to_string(scan.crate_root.join("src/runtime/submission.rs"))
        .expect("read submission finalization source");
    assert!(
        submission.contains("execution: Option<OperationExecution>"),
        "submission finalization must retain the admitted execution"
    );
    assert!(
        submission.contains("decision: &FinalizationDecision"),
        "submission finalization must consume the supervisor's immutable decision"
    );
    assert!(
        !submission.contains("pub(super) operation_id: Option<String>"),
        "submission finalization must not reconstruct identity from a detached string"
    );
    assert_eq!(
        dispatch.matches(".freeze(&execution, &result)").count()
            + execution.matches(".freeze(&execution, &result)").count(),
        4,
        "every dispatcher must freeze finalization through OperationSupervisor"
    );
    assert!(
        !submission.contains("fn submitted_terminal_status("),
        "submission projection must not retain a second terminal classifier"
    );

    let mut allocator_violations = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src")) {
        let relative = relative_path(&scan.repo_root, &path);
        let source = fs::read_to_string(&path).expect("read product source");
        for (line_index, line) in source.lines().enumerate() {
            if line.contains(".next_root_operation_id()")
                && !relative.ends_with("/src/runtime/admission.rs")
                && !relative.ends_with("/src/session/transaction.rs")
                && !relative.ends_with("/src/session/id.rs")
            {
                allocator_violations.push(format!(
                    "{relative}:{}: {}",
                    line_index + 1,
                    line.trim()
                ));
            }
            if line.contains(".next_child_operation_id()")
                && !relative.ends_with("/src/runtime/scheduler.rs")
                && !relative.ends_with("/src/session/id.rs")
            {
                allocator_violations.push(format!(
                    "{relative}:{}: {}",
                    line_index + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        allocator_violations.is_empty(),
        "operation identity allocator ownership was bypassed:\n{}",
        allocator_violations.join("\n")
    );
}

#[test]
fn runtime_host_owner_graph_and_first_writer_command_are_explicit() {
    let scan = SourceScan::new();
    let facade = fs::read_to_string(scan.crate_root.join("src/runtime/facade.rs"))
        .expect("read runtime facade source");
    assert!(
        facade.contains(
            "pub struct CodingAgentSession {\n    pub(super) runtime_host: RuntimeHost,\n}"
        ),
        "CodingAgentSession must remain a facade over one RuntimeHost composition root"
    );
    for legacy_field in [
        "pub(super) persistence: SessionPersistence",
        "pub(super) operation_control: OperationControl",
        "pub(super) event_service: EventService",
        "pub(super) snapshot_coordinator: Arc<SnapshotCoordinator>",
        "pub(super) client_service: ClientService",
    ] {
        assert!(
            !facade.contains(legacy_field),
            "facade must not retain owner authority: {legacy_field}"
        );
    }

    let owners = fs::read_to_string(scan.crate_root.join("src/runtime/owners.rs"))
        .expect("read runtime owners source");
    for owner in [
        "struct RuntimeHost",
        "struct OperationSupervisor",
        "struct EventHub",
        "struct ClientProjectionCoordinator",
    ] {
        assert!(owners.contains(owner), "missing runtime owner: {owner}");
    }
    assert!(
        owners.contains("finalizer: OperationFinalizer"),
        "OperationSupervisor must own terminal decision freezing"
    );
    let session_coordinator =
        fs::read_to_string(scan.crate_root.join("src/runtime/session_coordinator.rs"))
            .expect("read session coordinator source");
    for contract in [
        "struct SessionCoordinator",
        "struct SessionWriterCommand",
        "enum SessionMutation",
        "enum SessionWriterReply",
        "fn execute_writer_command(",
        "operation_id: String",
        "capability_generation: CapabilityGeneration",
    ] {
        assert!(
            session_coordinator.contains(contract),
            "missing writer protocol contract: {contract}"
        );
    }

    let dispatch = fs::read_to_string(scan.crate_root.join("src/runtime/dispatch.rs"))
        .expect("read operation dispatch source");
    assert!(
        dispatch.contains(".execute_writer_command("),
        "session mutation dispatch must enter the SessionCoordinator writer protocol"
    );
    assert!(
        !dispatch.contains("set_default_agent_profile_id("),
        "default-profile mutation must not bypass the writer command protocol"
    );
    for bypass in [
        "confirmation::reject_pending(",
        "session_navigation::fork(",
        "session_navigation::switch_active_leaf(",
        "session_navigation::set_tree_label(",
    ] {
        assert!(
            !dispatch.contains(bypass),
            "session mutation must not bypass the writer command protocol: {bypass}"
        );
    }
    let delegation_execution = fs::read_to_string(
        scan.crate_root
            .join("src/operations/delegation/execution.rs"),
    )
    .expect("read delegation execution source");
    assert!(
        delegation_execution.contains("session_coordinator: &mut SessionCoordinator"),
        "delegation approval must receive the narrow session owner"
    );
    assert!(
        !delegation_execution.contains("persistence: &mut SessionPersistence")
            && !delegation_execution
                .contains("pending_confirmations: &mut PendingDelegationConfirmationQueue"),
        "delegation approval must not split persistence and pending-queue authority"
    );

    let mut workflow_host_leaks = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src/operations")) {
        let source = fs::read_to_string(&path).expect("read workflow source");
        if source.contains("RuntimeHost") {
            workflow_host_leaks.push(relative_path(&scan.repo_root, &path));
        }
    }
    assert!(
        workflow_host_leaks.is_empty(),
        "RuntimeHost must not become a workflow service locator:\n{}",
        workflow_host_leaks.join("\n")
    );
}

#[test]
fn turn_transaction_stages_through_typed_writer_commands_without_repository_handles() {
    let scan = SourceScan::new();
    let transaction = fs::read_to_string(scan.crate_root.join("src/session/transaction.rs"))
        .expect("read transaction source");
    let struct_start = transaction
        .find("pub(crate) struct TurnTransaction")
        .expect("TurnTransaction declaration");
    let impl_start = transaction[struct_start..]
        .find("impl<G, C> TurnTransaction")
        .map(|offset| struct_start + offset)
        .expect("TurnTransaction implementation");
    let fields = &transaction[struct_start..impl_start];
    assert!(fields.contains("writer: SessionTransactionWriter"));
    assert!(fields.contains("session_id: String"));
    assert!(
        !fields.contains("store: SessionLogStore") && !fields.contains("handle: SessionHandle"),
        "workflow transaction must not retain raw repository authority"
    );
    for command in [
        "SessionTransactionWriterCommand::InitializeSession",
        "SessionTransactionWriterCommand::Checkpoint",
        "SessionTransactionWriterCommand::Finalize",
        "SessionTransactionWriterCommand::CommitSessionMutation",
    ] {
        assert!(
            transaction.contains(command),
            "missing typed transaction writer command: {command}"
        );
    }
    for transport in [
        "SESSION_WRITER_REGISTRY",
        "Weak<SessionTransactionWriterInner>",
        "writer_registry_key",
        "owners: AtomicUsize",
        "SessionWriterOwnerLease",
        "release_owner",
        "manifest_snapshot",
        "snapshot: Arc<Mutex<SessionManifest>>",
        "const SESSION_TRANSACTION_WRITER_CAPACITY: usize",
        "sync_channel::<SessionTransactionWriterEnvelope>",
        ".try_send(envelope)",
        "session transaction writer queue is full",
        "impl Drop for SessionTransactionWriterInner",
        "worker.join()",
        "let mut handle = handle",
        "execute_writer_command(&store, &mut handle, &mut write_lease, envelope.command)",
        "store.acquire_write_lease(&handle)?",
        "committed_session_sequence: Arc<AtomicU64>",
        "refresh_writer_handle(store, handle)",
        "outbox_records: Vec<DurableOutboxRecordCandidate>",
        "append_events_and_outbox(handle, &events, &outbox_records, write_lease)",
    ] {
        assert!(
            transaction.contains(transport),
            "missing bounded transaction writer transport contract: {transport}"
        );
    }
    let repository = fs::read_to_string(scan.crate_root.join("src/session/repository.rs"))
        .expect("read session repository source");
    for durable_cursor_contract in [
        ".commit(committed_through_session_sequence)",
        "outbox commit requires at least one sequenced session event",
        "references an event outside its commit batch",
    ] {
        assert!(
            repository.contains(durable_cursor_contract),
            "repository must own durable outbox cursor assignment: {durable_cursor_contract}"
        );
    }
    let service = fs::read_to_string(scan.crate_root.join("src/session/service.rs"))
        .expect("read session service source");
    assert!(
        service.contains("transaction_writer: SessionTransactionWriter"),
        "one SessionService owner must share one transaction writer transport"
    );
    assert!(
        service.contains("SessionTransactionWriter::new(store.clone(), handle.clone())"),
        "SessionService construction must acquire the canonical per-session writer"
    );
    let event_writer_start = service
        .find("pub(crate) struct SessionEventWriter")
        .expect("SessionEventWriter declaration");
    let event_writer_impl = service[event_writer_start..]
        .find("impl SessionEventWriter")
        .map(|offset| event_writer_start + offset)
        .expect("SessionEventWriter implementation");
    let event_writer_fields = &service[event_writer_start..event_writer_impl];
    assert!(event_writer_fields.contains("writer: SessionTransactionWriter"));
    assert!(event_writer_fields.contains("session_id: String"));
    assert!(event_writer_fields.contains("committed_session_sequence: Arc<AtomicU64>"));
    assert!(
        !event_writer_fields.contains("store: SessionLogStore")
            && !event_writer_fields.contains("handle: SessionHandle"),
        "authorization event writer must not retain raw repository authority"
    );
    assert!(
        service.contains("self.writer.append_checkpoint_events_with_receipt(events)")
            && service
                .contains("observe_commit_receipt(&self.committed_session_sequence, receipt)"),
        "authorization durable facts must use the shared bounded writer and retain its commit cursor"
    );
    let production_service = production_source(&sanitize_rust_source(&service));
    assert!(
        !production_service.contains(".store.append_events")
            && !production_service.contains(".store.update_manifest"),
        "SessionService must route live, bootstrap, and copy-target durable mutations through the writer"
    );
    let connection = fs::read_to_string(scan.crate_root.join("src/runtime/facade/connection.rs"))
        .expect("read runtime facade connection source");
    assert!(
        connection.contains("session_service.committed_session_sequence()")
            && !connection.contains("session_service.replay()"),
        "snapshot projection must consume the writer-derived commit cursor without replaying"
    );
    assert!(
        !production_service.contains("self.handle =")
            && !production_service.contains("target.handle ="),
        "repository handles remain read authority; mutable owner state must stay in the writer"
    );
    let repository_path = scan.crate_root.join("src/session/repository.rs");
    let transaction_path = scan.crate_root.join("src/session/transaction.rs");
    let mut durable_write_bypasses = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src")) {
        if path == repository_path || path == transaction_path {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read product source");
        let production = production_source(&sanitize_rust_source(&source));
        if production.contains(".append_events(") || production.contains(".update_manifest(") {
            durable_write_bypasses.push(relative_path(&scan.repo_root, &path));
        }
    }
    assert!(
        durable_write_bypasses.is_empty(),
        "production durable session writes must enter the writer owner; bypasses:\n{}",
        durable_write_bypasses.join("\n")
    );
    for mutation in [
        "set_tree_label",
        "set_default_agent_profile_id",
        "switch_active_leaf",
        "apply_startup_recovery",
        "append_durable_session_event",
    ] {
        assert!(
            service.contains(mutation),
            "missing expected migrated session mutation: {mutation}"
        );
    }
    let connection = fs::read_to_string(scan.crate_root.join("src/runtime/facade/connection.rs"))
        .expect("read runtime shutdown source");
    let drain = connection
        .find(".wait_for_active_operation_to_drain()")
        .expect("runtime shutdown drains operations");
    let close = connection
        .find(".session_coordinator.shutdown_writer()?")
        .expect("runtime shutdown closes session writer");
    let publish = connection
        .find(".emit_runtime_shutdown()")
        .expect("runtime shutdown publishes after writer close");
    assert!(
        drain < close && close < publish,
        "shutdown must drain operations, close/join the writer, then publish shutdown"
    );

    let mut workflow_repository_leaks = Vec::new();
    for path in rust_files_under(&scan.crate_root.join("src/operations")) {
        let source = fs::read_to_string(&path).expect("read workflow source");
        let production = production_source(&sanitize_rust_source(&source));
        if production.contains("SessionLogStore") || production.contains("SessionHandle") {
            workflow_repository_leaks.push(relative_path(&scan.repo_root, &path));
        }
    }
    assert!(
        workflow_repository_leaks.is_empty(),
        "workflow sources must not acquire raw session repository handles:\n{}",
        workflow_repository_leaks.join("\n")
    );
}

#[test]
fn delegated_child_flows_require_scheduler_lineage_admission() {
    let scan = SourceScan::new();
    for relative in [
        "src/operations/agent_invocation/mod.rs",
        "src/operations/team_invocation/mod.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read delegated operation wrapper source");
        assert!(source.contains("parent_capability_snapshot: OperationCapabilitySnapshot"));
        assert!(source.contains(".with_parent_capability_snapshot(parent_capability_snapshot)"));
    }

    for relative in [
        "src/operations/agent_invocation/runner.rs",
        "src/operations/team_invocation/runner.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .expect("read delegated child flow source");
        let production = production_source(&sanitize_rust_source(&source));
        assert!(
            production.contains("OperationScheduler::admit_child("),
            "delegated child flow must admit its child capability snapshot through the scheduler: {relative}"
        );
        assert!(
            production.contains("ActorId::ChildOperation("),
            "delegated child flow must construct an explicit parent lineage actor: {relative}"
        );
        assert!(
            production.contains("&self.operation_control"),
            "child admission must use the session runtime OperationControl owner: {relative}"
        );
        assert!(
            production.contains("delegation::execution::execute_agent(")
                && production.contains("delegation::execution::execute_team("),
            "nested delegation wrappers must use the shared admitted execution owner: {relative}"
        );
    }

    let scheduler = fs::read_to_string(scan.crate_root.join("src/runtime/scheduler.rs"))
        .expect("read scheduler source");
    let scheduler = production_source(&sanitize_rust_source(&scheduler));
    assert!(scheduler.contains(".begin_child_with_capability_generation("));
    assert!(scheduler.contains("OperationPermit::child("));

    let control = fs::read_to_string(scan.crate_root.join("src/runtime/control.rs"))
        .expect("read operation control source");
    for contract in [
        "children: Vec<ActiveChildOperation>",
        "owner_released: bool",
        "cancel_descendants",
        "remove_released_children_without_descendants",
        "remove_released_roots_without_descendants",
    ] {
        assert!(
            control.contains(contract),
            "operation control must own child lifetime contract `{contract}`"
        );
    }

    let delegation = fs::read_to_string(
        scan.crate_root
            .join("src/operations/delegation/execution.rs"),
    )
    .expect("read delegation execution source");
    let delegation = production_source(&sanitize_rust_source(&delegation));
    assert_eq!(
        delegation
            .matches("OperationScheduler::admit_child(")
            .count(),
        2
    );

    let dispatch_source = fs::read_to_string(scan.crate_root.join("src/runtime/dispatch.rs"))
        .expect("read operation dispatch source");
    let dispatch_production = production_source(&sanitize_rust_source(&dispatch_source));
    for entrypoint in [
        "crate::operations::agent_invocation::run(",
        "crate::operations::team_invocation::run(",
    ] {
        let call = dispatch_production.find(entrypoint).unwrap_or_else(|| {
            panic!("missing canonical child operation entrypoint: {entrypoint}")
        });
        let call_region = &dispatch_production[call..dispatch_production.len().min(call + 512)];
        assert!(
            call_region.contains("snapshot.operation_id.clone()"),
            "canonical root dispatch must pass its admitted operation id to child flow: {entrypoint}"
        );
    }
}

#[test]
fn operation_tree_fault_evidence_cannot_create_parallel_production_owners() {
    let scan = SourceScan::new();
    let scheduler = fs::read_to_string(scan.crate_root.join("src/runtime/scheduler.rs"))
        .expect("read scheduler owner");
    assert_eq!(
        scheduler
            .matches("pub(crate) struct OperationScheduler")
            .count(),
        1,
        "operation admission must retain one scheduler owner"
    );

    let authorization = fs::read_to_string(scan.crate_root.join("src/services/authorization.rs"))
        .expect("read authorization owner");
    let authorization_production = production_source(&sanitize_rust_source(&authorization));
    assert_eq!(
        authorization_production
            .matches("struct AuthorizationService")
            .count(),
        1,
        "authorization waiters must retain one service owner"
    );
    assert_eq!(
        authorization_production
            .matches("pending: BTreeMap<String, PendingAuthorization>")
            .count(),
        1,
        "authorization waiters must retain one registry"
    );

    for relative in [
        "src/runtime/control.rs",
        "src/runtime/scheduler.rs",
        "src/runtime/finalization.rs",
        "src/services/authorization.rs",
        "src/services/event.rs",
        "src/session/transaction.rs",
    ] {
        let source = fs::read_to_string(scan.crate_root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        let production = production_source(&sanitize_rust_source(&source));
        for forbidden in ["FaultQueue", "FaultCommand", "fault_fallback", "fault_mode"] {
            assert!(
                !production.contains(forbidden),
                "fault evidence leaked production fallback `{forbidden}` into {relative}"
            );
        }
    }
}

fn discover_adapter_candidates(scan: &SourceScan) -> HashSet<String> {
    let sources = rust_files_under(&scan.crate_root.join("src"))
        .into_iter()
        .map(|path| {
            let relative = relative_path(&scan.repo_root, &path);
            let source = fs::read_to_string(path).expect("read production source");
            (relative, source)
        })
        .collect::<Vec<_>>();
    let borrowed = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    discover_adapter_candidates_from_sources(&borrowed)
}

fn discover_adapter_candidates_from_sources(sources: &[(&str, &str)]) -> HashSet<String> {
    sources
        .iter()
        .filter_map(|(path, source)| {
            let sanitized = sanitize_rust_source(source);
            let production = production_source(&sanitized);
            let is_operation_boundary = production.contains("CodingAgentOperation")
                && (production.contains(".run(")
                    || production.contains(".run_internal(")
                    || production.contains(".submit_internal(")
                    || production.contains("session.run("));
            let is_connection_boundary = production.contains("CodingAgentClientConnection")
                || production.contains(".prepare_client_submission(")
                || production.contains(".prepare_client_submission_internal(")
                || production.contains(".reconnect(")
                || production.contains(".acknowledge(");
            let is_event_boundary = (production.contains("ProductEvent")
                || production.contains("CodingAgentProductEvent"))
                && (production.contains("ProtocolEvent")
                    || production.contains("UiEvent")
                    || production.contains("EventAdapter")
                    || production.contains("EventBridge"));
            let is_mode_or_output_boundary = production.contains("CliOutput")
                && (production.contains("run_") && production.contains("mode"));
            (is_operation_boundary
                || is_connection_boundary
                || is_event_boundary
                || is_mode_or_output_boundary)
                .then(|| (*path).to_owned())
        })
        .collect()
}

fn production_source(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut depth = 0isize;
    let mut test_item_depths = Vec::new();
    let mut pending_test_cfg = false;
    let mut pending_attribute_depth = 0isize;
    for line in source.lines() {
        let trimmed = line.trim();
        let test_cfg = matches!(trimmed, "#[cfg(test)]" | "#[cfg(any())]");
        if test_cfg {
            pending_test_cfg = true;
        }
        let gated = pending_test_cfg || !test_item_depths.is_empty();
        if pending_test_cfg && !test_cfg {
            let attribute_delta =
                trimmed
                    .chars()
                    .fold(0isize, |attribute_depth, character| match character {
                        '[' => attribute_depth + 1,
                        ']' => attribute_depth - 1,
                        _ => attribute_depth,
                    });
            if pending_attribute_depth > 0 {
                pending_attribute_depth += attribute_delta;
            } else if trimmed.starts_with("#[") {
                pending_attribute_depth = attribute_delta.max(0);
            } else if trimmed.contains('{') {
                test_item_depths.push(depth + 1);
                pending_test_cfg = false;
            } else if trimmed.ends_with(';') || trimmed.ends_with(',') {
                pending_test_cfg = false;
            }
        }
        if !gated {
            output.push_str(line);
        }
        output.push('\n');
        depth += brace_delta(line);
        test_item_depths.retain(|item_depth| depth >= *item_depth);
    }
    output
}

#[test]
fn production_code_does_not_import_testing_facades() {
    let scan = SourceScan::new();
    let mut violations = Vec::new();

    for path in rust_files_under(&scan.crate_root.join("src")) {
        let relative = relative_path(&scan.repo_root, &path);
        if relative.contains("/src/internal_tests/") {
            continue;
        }
        let raw = fs::read_to_string(&path).expect("read product source");
        let production = production_source(&sanitize_rust_source(&raw));
        for (line_index, line) in production.lines().enumerate() {
            if line.contains("::api::testing") {
                violations.push(format!("{relative}:{}: {}", line_index + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "test-support facades must not enter product production code:\n{}",
        violations.join("\n")
    );
}

#[test]
fn product_test_support_and_lint_exceptions_are_explicitly_scoped() {
    let scan = SourceScan::new();
    let manifest =
        fs::read_to_string(scan.crate_root.join("Cargo.toml")).expect("read coding-agent manifest");
    let library =
        fs::read_to_string(scan.crate_root.join("src/lib.rs")).expect("read crate root source");

    assert!(
        !manifest.contains("test-harness"),
        "the unused test-harness feature must not return"
    );
    assert!(
        library.contains("#[cfg(any(test, feature = \"test-support\"))]"),
        "test helpers must require cfg(test) or the explicit non-default test-support feature"
    );
    assert!(
        !library.contains("debug_assertions"),
        "ordinary debug builds must not include environment/provider mutation helpers"
    );
    for lint in [
        "result_large_err",
        "large_enum_variant",
        "too_many_arguments",
        "collapsible_if",
    ] {
        assert!(
            !library.contains(&format!("#![allow(clippy::{lint})]")),
            "crate-wide Clippy exception must not return: {lint}"
        );
    }
}

fn validate_adapter_classifications(
    discovered: &HashSet<String>,
    classifications: &[AdapterClassification],
) -> Vec<String> {
    let mut violations = Vec::new();
    let mut classified = HashSet::new();
    for classification in classifications {
        if classification.rationale.trim().is_empty() {
            violations.push(format!(
                "classification has empty rationale: {}",
                classification.path
            ));
        }
        if !classified.insert(classification.path.to_owned()) {
            violations.push(format!(
                "candidate classified more than once: {}",
                classification.path
            ));
        }
    }
    for path in discovered.difference(&classified) {
        violations.push(format!("unclassified adapter candidate: {path}"));
    }
    for path in classified.difference(discovered) {
        violations.push(format!("stale adapter classification: {path}"));
    }
    violations.sort();
    violations
}

#[test]
fn adapter_scanner_fixture_matrix_is_sanitized_and_structural() {
    let fixture = r#"
        // session.prompt("comment")
        let text = ".prompt(";
        let ch = '.';
        #[cfg(any())]
        use crate::runtime::client;
        #[cfg(any())]
        mod archived_tests { fn hidden(session: &Session) { session.prompt("archived"); } }
        #[cfg(test)]
        mod tests { fn hidden(session: &Session) { session.prompt("test"); } }
        session
            .prompt("multiline")
            ;
        (session).prompt("parenthesized");
        other.prompt("legitimate");
    "#;
    let sanitized = sanitize_rust_source(fixture);
    let production = production_source(&sanitized);
    assert_eq!(production.matches(".prompt(").count(), 3);
    assert!(production.contains("session\n            .prompt("));
    assert!(production.contains("(session).prompt("));
    assert!(!production.contains("comment"));
    assert!(!production.contains("runtime::client"));
    assert!(!production.contains("archived"));
    assert!(production.contains("other.prompt("));
}

#[test]
fn adapter_discovery_fixture_rejects_unclassified_and_stale_ownership() {
    let sources = [
        (
            "src/protocol/new_transport.rs",
            "pub async fn run_new_transport(session: &mut CodingAgentSession) { session.run(CodingAgentOperation::Prompt(todo!())).await; }",
        ),
        (
            "src/protocol/comment_only.rs",
            r#"// CodingAgentSession::run(CodingAgentOperation)
               const TEXT: &str = "CodingAgentClientConnection";"#,
        ),
        (
            "src/helpers/near_miss.rs",
            "fn run_modeled_value() -> usize { 1 }",
        ),
        (
            "src/protocol/test_only.rs",
            "#[cfg(test)] mod tests { fn adapter(session: &mut CodingAgentSession) { session.run(todo!()); } }",
        ),
    ];
    let discovered = discover_adapter_candidates_from_sources(&sources);
    assert_eq!(
        discovered,
        HashSet::from(["src/protocol/new_transport.rs".to_owned()])
    );

    let unclassified = validate_adapter_classifications(&discovered, &[]);
    assert!(
        unclassified
            .iter()
            .any(|violation| violation.contains("unclassified"))
    );

    let stale = validate_adapter_classifications(
        &HashSet::new(),
        &[AdapterClassification {
            path: "src/protocol/removed.rs",
            rationale: "legacy transport boundary",
        }],
    );
    assert!(stale.iter().any(|violation| violation.contains("stale")));
}

#[test]
fn session_method_inventory_accepts_multiline_impl_and_signature() {
    let fixture = tempfile::tempdir().expect("create session method fixture");
    let source_path = fixture.path().join("session.rs");
    fs::write(
        &source_path,
        r#"
            impl CodingAgentSession
            {
                pub async fn prompt(
                    &mut self,
                    prompt: &str,
                ) -> Result<(), Error> {
                    todo!()
                }
            }
        "#,
    )
    .expect("write session method fixture");

    let mut methods = Vec::new();
    collect_coding_agent_session_methods(fixture.path(), &source_path, &mut methods);
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].name, "prompt");
    assert_eq!(methods[0].visibility, "pub");
}
