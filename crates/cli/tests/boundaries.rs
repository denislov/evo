use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

#[derive(Default)]
struct PathRootCollector {
    roots: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for PathRootCollector {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(root) = path.segments.first() {
            self.roots.insert(root.ident.to_string());
        }
        visit::visit_path(self, path);
    }
}

fn rust_fragment_path_roots(source: &str) -> BTreeSet<String> {
    let mut collector = PathRootCollector::default();
    if let Ok(file) = syn::parse_file(source) {
        collector.visit_file(&file);
    } else {
        let block = syn::parse_str::<syn::Block>(source)
            .expect("boundary source fragment must remain valid Rust");
        collector.visit_block(&block);
    }
    collector.roots
}

fn assert_no_private_dependency_paths(source: &str, owner: &str) {
    let roots = rust_fragment_path_roots(source);
    for forbidden in ["ai", "agent_core"] {
        assert!(
            !roots.contains(forbidden),
            "{owner} bypasses product ownership through Rust path root {forbidden}"
        );
    }
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .expect("cli must live under the workspace crates directory")
        .to_path_buf()
}

fn dependency_names(manifest: &str) -> BTreeSet<&str> {
    let dependencies = manifest
        .split_once("[dependencies]")
        .expect("cli manifest must have a dependencies table")
        .1
        .split("\n[")
        .next()
        .expect("dependencies table must be readable");

    dependencies
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty() && !line.starts_with('#'))
                .then(|| line.split_once('=').map(|(name, _)| name.trim()))
                .flatten()
        })
        .collect()
}

#[test]
fn package_and_binary_identity_are_explicit() {
    let manifest =
        fs::read_to_string(crate_root().join("Cargo.toml")).expect("read the cli package manifest");

    assert!(manifest.contains("name = \"cli\""));
    assert!(manifest.contains("version.workspace = true"));
    assert!(manifest.contains("edition = \"2024\""));
    assert!(manifest.contains("publish = false"));
    assert!(manifest.contains("[[bin]]"));
    assert!(manifest.contains("name = \"coding-agent\""));
    assert!(manifest.contains("path = \"src/main.rs\""));
    assert!(
        !crate_root().join("src/lib.rs").exists(),
        "cli must not expose an accidental library SDK"
    );
}

#[test]
fn workspace_has_one_real_coding_agent_binary_owner() {
    let repo_root = repo_root();
    let workspace =
        fs::read_to_string(repo_root.join("Cargo.toml")).expect("read the workspace manifest");
    let product_root = repo_root.join("crates/coding-agent");

    assert!(
        workspace.contains("\"crates/cli\""),
        "the workspace must activate cli"
    );
    assert!(
        !product_root.join("src/main.rs").exists(),
        "coding-agent must be a library-only product crate"
    );

    let product_manifest = fs::read_to_string(product_root.join("Cargo.toml"))
        .expect("read the coding-agent manifest");
    assert!(
        !product_manifest.contains("[[bin]]"),
        "the product crate must not retain an explicit binary target"
    );
}

#[test]
fn adapter_dependencies_do_not_bypass_product_ownership() {
    let manifest =
        fs::read_to_string(crate_root().join("Cargo.toml")).expect("read the cli package manifest");
    let dependencies = dependency_names(&manifest);
    let expected = BTreeSet::from([
        "libc",
        "coding-agent",
        "tui",
        "serde",
        "serde_json",
        "syntect",
        "thiserror",
        "tokio",
        "time",
        "uuid",
    ]);

    assert_eq!(dependencies, expected);
    assert!(!dependencies.contains("ai"));
    assert!(!dependencies.contains("agent-core"));
}

#[test]
fn entrypoint_owns_process_io_without_forwarding_or_copying_the_runner() {
    let source =
        fs::read_to_string(crate_root().join("src/main.rs")).expect("read the cli entrypoint");

    for required in [
        "std::env::args().skip(1)",
        "parse_args",
        "read_text_from",
        "CodingAgentInteractiveStartup::resolve",
        "CodingAgentPromptExecution::prepare",
        "cli::headless::run",
        "CodingAgentApplicationStartup::resolve",
        "rpc::run_rpc_mode_stdio",
        "list_models_output",
        "std::io::stdin().is_terminal()",
        "interactive::run_interactive_mode",
        "print!",
        "eprint!",
        "std::process::exit(output.exit_code)",
    ] {
        assert!(
            source.contains(required),
            "cli entrypoint must own process behavior: {required}"
        );
    }
    for forbidden in [
        "std::process::Command",
        "Command::new",
        "cargo run",
        "fn run_cli(",
        "fn run_cli_stdio(",
        "run_cli_stdio_with_interactive",
        "run_interactive_invocation",
        "run_headless_invocation",
        "run_rpc_invocation",
        "coding_agent::api::cli",
    ] {
        assert!(
            !source.contains(forbidden),
            "cli entrypoint must not forward or duplicate the existing runner: {forbidden}"
        );
    }
}

#[test]
fn process_contracts_execute_the_real_cli_binary_without_product_test_backdoors() {
    let source = fs::read_to_string(crate_root().join("tests/cli_process.rs"))
        .expect("read real CLI process contract tests");

    for required in [
        "CARGO_BIN_EXE_coding-agent",
        "EVO_DIR",
        "Stdio::null()",
        "help_is_rendered_by_the_real_binary",
        "version_is_rendered_by_the_real_binary",
        "model_list_text_is_rendered_without_starting_a_prompt",
        "model_list_json_honors_the_provider_filter",
        "model_list_is_read_only_for_session_selection",
        "print_mode_rejects_a_missing_prompt_on_stderr",
        "unknown_model_uses_the_safe_public_error",
        "default_invocation_routes_to_the_interactive_adapter",
    ] {
        assert!(
            source.contains(required),
            "cli must retain real-process evidence for {required}"
        );
    }
    assert_no_private_dependency_paths(&source, "CLI process tests");
    for forbidden in [
        "coding_agent::",
        "FauxProvider",
        "CliRunOptions",
        "run_cli_with_options",
    ] {
        assert!(
            !source.contains(forbidden),
            "CLI process tests bypass product ownership through {forbidden}"
        );
    }
}

#[test]
fn terminal_projection_executes_the_shared_product_fixture() {
    let source = fs::read_to_string(crate_root().join("src/interactive/event_bridge.rs"))
        .expect("read terminal product projection");

    for required in [
        "#[cfg(test)]\nmod cross_adapter_tests",
        "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json",
        "shared_cross_adapter_fixture_matches_interactive_product_state_exactly",
        "CodingAgentClientProjection::new",
        "interactive.apply_product_event(&event)",
        "snapshot-backed interactive projection",
        "&shared",
    ] {
        assert!(
            source.contains(required),
            "enabled terminal projection evidence must retain {required}"
        );
    }
    assert_eq!(
        source
            .matches("shared_cross_adapter_fixture_matches_interactive_product_state_exactly")
            .count(),
        1,
        "the cross-adapter assertion must not survive only as disabled legacy test text"
    );
}

#[test]
fn cli_local_unit_tests_are_executable_without_product_test_support() {
    for relative in [
        "src/interactive/git_branch.rs",
        "src/interactive/key_hints.rs",
        "src/interactive/input.rs",
        "src/interactive/prompt_task.rs",
        "src/interactive/render.rs",
        "src/interactive/session_actions.rs",
        "src/interactive/transcript.rs",
        "src/interactive/transient_overlay.rs",
        "src/interactive/tree_selector.rs",
    ] {
        let source = fs::read_to_string(crate_root().join(relative))
            .unwrap_or_else(|error| panic!("read CLI-local test owner {relative}: {error}"));
        let test_marker = [
            "#[cfg(test)]\nmod tests",
            "#[cfg(test)]\nmod view_state_tests",
            "#[cfg(test)]\nmod hydration_tests",
            "#[cfg(test)]\nmod ownership_tests",
        ]
        .into_iter()
        .find(|marker| source.contains(marker))
        .expect("enabled CLI-local test module marker");
        assert!(
            source.contains(test_marker),
            "CLI-local tests in {relative} must compile and execute"
        );
        let tests = source
            .split(test_marker)
            .nth(1)
            .expect("enabled CLI-local test module");
        assert_no_private_dependency_paths(tests, &format!("CLI-local tests in {relative}"));
        for forbidden in [
            "coding_agent::test_support",
            "FauxProvider",
            "ProviderGuard",
        ] {
            assert!(
                !tests.contains(forbidden),
                "CLI-local tests in {relative} bypass ownership through {forbidden}"
            );
        }
    }
}

#[test]
fn cli_sources_do_not_hide_migration_debt_behind_never_cfg() {
    let mut pending = vec![crate_root().join("src")];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("read CLI source directory {}: {error}", path.display()))
        {
            let entry = entry.expect("read CLI source entry");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read CLI source {}: {error}", path.display()));
            assert!(
                !source.contains("#[cfg(any())]"),
                "{} must delete retired migration code instead of hiding it behind cfg(any())",
                path.display()
            );
        }
    }
}

#[test]
fn interactive_app_and_loop_legacy_tests_are_classified() {
    let app = fs::read_to_string(crate_root().join("src/interactive/app.rs"))
        .expect("read interactive app owner");
    let loop_source = fs::read_to_string(crate_root().join("src/interactive/loop.rs"))
        .expect("read interactive loop owner");
    let loop_tests = fs::read_to_string(crate_root().join("src/interactive/loop_tests.rs"))
        .expect("read enabled interactive loop tests");
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read cli manifest");

    for source in [&app, &loop_source] {
        assert!(
            !source.contains("#[cfg(any())]\nmod tests")
                && !source.contains("#[cfg(any())]\n#[allow(clippy::items_after_test_module)]"),
            "app/loop legacy tests must be enabled under their final owner or deleted"
        );
    }
    for retired in [
        "build_prompt_context_uses_config_defaults_and_auth",
        "real_prompt_partial_commit_returns_recovery_pending_without_terminal_event",
        "real_fork_failure_preserves_source_owner_subscriber_and_target_through_prompt_task_done",
        "FauxProvider",
        "PromptRunOptions",
        "SessionRunOptions",
    ] {
        assert!(
            !loop_tests.contains(retired),
            "enabled loop tests retained private product/runtime evidence through {retired}"
        );
    }
    for required in [
        "terminal_progress_transitions_through_the_owned_terminal",
        "presentation_mode_maps_to_the_owned_terminal_lifecycle_mode",
        "coalesced_stream_updates_do_not_bypass_the_render_interval",
        "transient_overlay_roles_keep_independent_geometry_and_capture_policy",
        "fullscreen_slash_assistance_stays_aligned_across_resizes",
        "fullscreen_file_assistance_is_above_and_aligned_with_the_composer",
    ] {
        assert!(
            loop_tests.contains(required),
            "enabled loop owner must retain {required}"
        );
    }
    assert_no_private_dependency_paths(&loop_tests, "enabled loop tests");
    for forbidden in [
        "coding_agent::test_support",
        "crate::runtime",
        "crate::events",
        "FauxProvider",
        "ProviderGuard",
    ] {
        assert!(
            !loop_tests.contains(forbidden),
            "enabled loop tests bypass final ownership through {forbidden}"
        );
    }
    assert!(
        manifest.contains("tui = { path = \"../tui\", features = [\"test-support\"] }"),
        "cli must own virtual-terminal test support without reintroducing it into the product crate"
    );
}

#[test]
fn interactive_reducer_fixtures_execute_in_the_cli_without_private_runtime_seeds() {
    let interactive_mod = fs::read_to_string(crate_root().join("src/interactive/mod.rs"))
        .expect("read interactive module owner");
    let interactive_app = fs::read_to_string(crate_root().join("src/interactive/app.rs"))
        .expect("read interactive application owner");
    assert!(
        !interactive_mod.contains("test_harness") && !interactive_app.contains("mod test_harness"),
        "cli must not retain the private-provider scripted interactive harness"
    );
    for required in [
        "#[cfg(test)]\nmod event_bridge_tests;",
        "#[cfg(test)]\nmod transcript_tests;",
    ] {
        assert!(
            interactive_mod.contains(required),
            "cli must execute migrated interactive reducer evidence: {required}"
        );
    }

    let fixtures = [
        (
            "src/interactive/event_bridge_tests.rs",
            [
                "coding_event_bridge_maps_assistant_events",
                "coding_event_bridge_maps_tool_events",
                "coding_event_bridge_maps_delegation_confirmation_events",
                "coding_event_bridge_maps_self_healing_edit_events",
                "ui_events_apply_to_transcript",
            ]
            .as_slice(),
        ),
        (
            "src/interactive/transcript_tests.rs",
            [
                "transcript_scrolls_within_bounds",
                "transcript_keeps_scrolled_view_locked_when_new_output_arrives",
                "transcript_revision_changes_only_on_real_mutation",
                "tool_event_closes_current_assistant_before_next_assistant_delta",
            ]
            .as_slice(),
        ),
    ];
    for (relative, required_tests) in fixtures {
        let source = fs::read_to_string(crate_root().join(relative)).unwrap_or_else(|error| {
            panic!("read migrated CLI reducer fixture {relative}: {error}")
        });
        for required in required_tests {
            assert!(
                source.contains(required),
                "migrated CLI reducer fixture {relative} must retain {required}"
            );
        }
        assert_no_private_dependency_paths(
            &source,
            &format!("migrated CLI reducer fixture {relative}"),
        );
        for forbidden in [
            "coding_agent::test_support",
            "ProductEventDraft",
            "FauxProvider",
            "ProviderGuard",
            "CliRunOptions",
            "run_scripted_interactive",
        ] {
            assert!(
                !source.contains(forbidden),
                "migrated CLI reducer fixture {relative} bypasses ownership through {forbidden}"
            );
        }
    }
}

#[test]
fn process_output_and_dispatch_are_cli_owned() {
    let output =
        fs::read_to_string(crate_root().join("src/output.rs")).expect("read CLI output owner");
    let product_lib = fs::read_to_string(repo_root().join("crates/coding-agent/src/lib.rs"))
        .expect("read product facade");
    let product_application =
        fs::read_to_string(repo_root().join("crates/coding-agent/src/app/application.rs"))
            .expect("read private product application resolver");

    assert!(output.contains("pub(crate) struct CliOutput"));
    assert!(output.contains("pub exit_code: i32"));
    for retired in [
        "pub mod cli {",
        "CliOutput",
        "run_interactive_invocation",
        "run_headless_invocation",
        "run_rpc_invocation",
    ] {
        assert!(
            !product_lib.contains(retired),
            "product facade retained process surface {retired}"
        );
    }
    for forbidden in [
        "CliOutput",
        "FnOnce(",
        "Future<",
        "std::process",
        "std::io::",
    ] {
        assert!(
            !product_application.contains(forbidden),
            "private product resolution regained process ownership through {forbidden}"
        );
    }
    assert!(
        !repo_root().join("crates/coding-agent/src/app/cli").exists(),
        "the retired product CLI owner bucket must stay deleted"
    );
}

#[test]
fn rpc_and_wire_protocol_are_owned_locally_behind_categorized_product_api() {
    let cli_root = crate_root();
    let repo_root = repo_root();
    let product_root = repo_root.join("crates/coding-agent");
    let required = [
        "src/rpc/mod.rs",
        "src/rpc/commands.rs",
        "src/rpc/event_queue.rs",
        "src/rpc/events.rs",
        "src/rpc/limits.rs",
        "src/rpc/prompt.rs",
        "src/rpc/state.rs",
        "src/rpc/stats.rs",
        "src/rpc/wire.rs",
        "src/protocol/events.rs",
        "src/protocol/events_tests.rs",
        "src/protocol/json.rs",
        "src/protocol/jsonl.rs",
        "src/protocol/types.rs",
        "src/protocol/version.rs",
    ];

    for relative in required {
        assert!(
            cli_root.join(relative).is_file(),
            "cli must own migrated RPC/protocol source {relative}"
        );
    }
    for retired in [
        "src/adapters/mod.rs",
        "src/adapters/events.rs",
        "src/adapters/rpc/mod.rs",
        "src/protocol/mod.rs",
        "src/protocol/types.rs",
        "tests/events_snapshot/protocol_events.rs",
        "tests/recovery/protocol_sessions.rs",
        "tests/rpc/mode.rs",
    ] {
        assert!(
            !product_root.join(retired).exists(),
            "coding-agent must not retain application protocol owner {retired}"
        );
    }

    let mut moved_sources = Vec::new();
    collect_rust_sources(&cli_root.join("src/rpc"), &mut moved_sources);
    collect_rust_sources(&cli_root.join("src/protocol"), &mut moved_sources);
    for path in &moved_sources {
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read migrated source {}: {error}", path.display()));
        for forbidden in [
            "ai::",
            "agent_core::",
            "coding_agent::api::protocol",
            "crate::adapters::",
            "crate::app::",
            "crate::runtime::",
            "crate::services::",
            "crate::authorization::",
            "crate::events::",
        ] {
            assert!(
                !source.contains(forbidden),
                "migrated CLI source {} bypasses the product facade through {forbidden}",
                path.display()
            );
        }
        for line in source
            .lines()
            .filter(|line| line.contains("coding_agent::"))
        {
            assert!(
                line.contains("coding_agent::api::"),
                "migrated CLI source {} uses an uncategorized product import: {}",
                path.display(),
                line.trim()
            );
        }
    }

    let event_mapper = cli_root.join("src/protocol/events.rs");
    let mapper_source = fs::read_to_string(&event_mapper).expect("read CLI protocol event mapper");
    assert!(mapper_source.contains("pub struct CodingProtocolEventAdapter"));
    assert!(mapper_source.contains("push_product_event"));

    let mut workspace_sources = Vec::new();
    collect_rust_sources(&repo_root.join("crates"), &mut workspace_sources);
    let mapper_owners = workspace_sources
        .into_iter()
        .filter(|path| {
            path.components()
                .any(|component| component.as_os_str() == "src")
        })
        .filter(|path| {
            fs::read_to_string(path).is_ok_and(|source| {
                source.contains(
                    ["pub struct ", "CodingProtocolEventAdapter"]
                        .concat()
                        .as_str(),
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mapper_owners,
        vec![event_mapper],
        "ProductEvent-to-wire projection must have one application-owned mapper"
    );
}

#[test]
fn rpc_negotiation_and_wire_version_stay_cli_owned() {
    let types = fs::read_to_string(crate_root().join("src/protocol/types.rs"))
        .expect("read CLI protocol types");
    let version = fs::read_to_string(crate_root().join("src/protocol/version.rs"))
        .expect("read CLI protocol version");
    let commands =
        fs::read_to_string(crate_root().join("src/rpc/commands.rs")).expect("read RPC commands");
    let state = fs::read_to_string(crate_root().join("src/rpc/state.rs")).expect("read RPC state");
    let stats = fs::read_to_string(crate_root().join("src/rpc/stats.rs")).expect("read RPC stats");

    assert!(types.contains("Hello {"));
    assert!(version.contains("pub const RPC_PROTOCOL_VERSION"));
    assert!(commands.contains("is_compatible_with(RPC_PROTOCOL_VERSION"));
    assert!(state.contains("negotiated_protocol"));
    assert!(stats.contains("negotiated_protocol"));
}

#[test]
fn rpc_fixture_evidence_executes_in_the_cli_without_private_product_seeds() {
    let process = fs::read_to_string(crate_root().join("tests/rpc_stdio.rs"))
        .expect("read real RPC stdio process tests");
    let projection = fs::read_to_string(crate_root().join("src/protocol/events_tests.rs"))
        .expect("read CLI-local protocol projection tests");

    for required in [
        "CARGO_BIN_EXE_coding-agent",
        "rpc_stdio_recovers_from_invalid_input_and_negotiates_before_state",
        "rpc_stdio_flushes_before_eof_and_returns_idempotent_detach_status",
        "fixtures/rpc-hello-response.json",
        "identifier_bytes",
        "image_count",
        "json_depth",
        "protocol_already_negotiated",
        "already_detached",
    ] {
        assert!(
            process.contains(required),
            "real RPC process evidence must retain {required}"
        );
    }
    for required in [
        "coding_event_adapter_maps_prompt_sequence_to_protocol_events",
        "coding_event_adapter_maps_session_write_failure_state",
        "coding_event_adapter_maps_self_healing_edit_lifecycle_to_protocol_events",
        "coding_event_adapter_maps_profile_and_delegation_lifecycle_to_protocol_events",
        "coding_event_adapter_maps_tool_events_to_protocol_events",
        "lifecycle_wire_values_are_additive_and_exact",
        "product_event_protocol_adapter_does_not_emit_flow_node_fields",
    ] {
        assert!(
            projection.contains(required),
            "CLI-local ProductEvent-to-wire evidence must retain {required}"
        );
    }
    for source in [&process, &projection] {
        for forbidden in [
            "ai::",
            "agent_core::",
            "coding_agent::test_support",
            "coding_agent::api::protocol",
            "ProductEventDraft",
            "FauxProvider",
            "ProviderGuard",
            "CliRunOptions",
            "run_rpc_mode_for_io",
        ] {
            assert!(
                !source.contains(forbidden),
                "RPC adapter evidence bypasses the public product boundary through {forbidden}"
            );
        }
    }
}

fn collect_rust_sources(path: &Path, sources: &mut Vec<PathBuf>) {
    let metadata = fs::metadata(path)
        .unwrap_or_else(|error| panic!("inspect source path {}: {error}", path.display()));
    if metadata.is_file() {
        if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            sources.push(path.to_path_buf());
        }
        return;
    }

    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read source directory {}: {error}", path.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("collect source directory entries");
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        collect_rust_sources(&entry.path(), sources);
    }
}

#[test]
fn headless_presentation_is_owned_locally_without_product_adapter_routing() {
    let source = fs::read_to_string(crate_root().join("src/cli/headless.rs"))
        .expect("read the cli headless presentation owner");

    for required in [
        "CodingAgentPromptExecution",
        "PromptTurnMode::Print => run_print(execution).await",
        "PromptTurnMode::Json => run_json(execution, cwd).await",
        "CodingAgentPromptExecutionUpdate::Event",
        "CodingProtocolEventAdapter",
        "/fixtures/client_projection/headless-wire-events.json",
        "product_owned_fixture_preserves_complete_headless_jsonl_order_and_shape",
        "public_prompt_failure_keeps_stdout_valid_jsonl_and_stderr_safe",
        "tool_authorization_required",
        "tool_authorization_denied",
        "tool_execution_start",
        "tool_execution_end",
    ] {
        assert!(
            source.contains(required),
            "cli must retain headless presentation ownership: {required}"
        );
    }
    for forbidden in [
        "adapters::print",
        "adapters::json",
        "run_print_mode",
        "PrintModeOptions",
        "ai::",
        "agent_core::",
        "ProductEventDraft",
        "FauxProvider",
        "ProviderGuard",
        "test_support",
    ] {
        assert!(
            !source.contains(forbidden),
            "cli must not route through a product presentation adapter: {forbidden}"
        );
    }
}
