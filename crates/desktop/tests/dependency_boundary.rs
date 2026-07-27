use std::{collections::BTreeSet, fs, path::PathBuf};

fn manifest() -> toml::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let source = fs::read_to_string(path).expect("desktop manifest should be readable");
    toml::from_str(&source).expect("desktop manifest should be valid TOML")
}

fn dependency_names(value: &toml::Value, names: &mut BTreeSet<String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if (key == "dependencies" || key == "dev-dependencies" || key == "build-dependencies")
            && let Some(dependencies) = child.as_table()
        {
            names.extend(dependencies.keys().cloned());
        }
        dependency_names(child, names);
    }
}

#[test]
fn desktop_depends_on_product_facade_without_bypassing_runtime_layers() {
    let mut names = BTreeSet::new();
    dependency_names(&manifest(), &mut names);

    assert!(names.contains("coding-agent"));
    for forbidden in ["ai", "agent-core", "tui"] {
        assert!(
            !names.contains(forbidden),
            "desktop must not depend directly on {forbidden}"
        );
    }
}

#[test]
fn unstable_ui_dependencies_are_exactly_pinned() {
    let manifest = manifest();
    let dependencies = manifest["dependencies"]
        .as_table()
        .expect("dependencies table");
    assert_eq!(
        dependencies["gpui-component"]["rev"].as_str(),
        Some("bc174a7ec4534b2a4174fddde314b38d30d69093")
    );
    assert_eq!(
        dependencies["gpui-component"]["git"].as_str(),
        Some("https://github.com/longbridge/gpui-component.git")
    );

    let targets = manifest["target"].as_table().expect("target table");
    for target in [
        "cfg(target_os = \"linux\")",
        "cfg(target_os = \"macos\")",
        "cfg(target_os = \"windows\")",
    ] {
        assert_eq!(
            targets[target]["dependencies"]["gpui"]["git"].as_str(),
            Some("https://github.com/zed-industries/zed.git"),
            "GPUI must use the AccessKit-capable upstream for {target}"
        );
        assert!(
            targets[target]["dependencies"]
                .get("gpui_platform")
                .is_some()
        );
    }

    let lock_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("Cargo.lock");
    let lock = fs::read_to_string(lock_path).expect("workspace lockfile should be readable");
    assert!(lock.contains(
        "git+https://github.com/zed-industries/zed.git#30730a305ae235f3be44643d5895e142048ef701"
    ));
    assert!(lock.contains(
        "git+https://github.com/longbridge/gpui-component.git?rev=bc174a7ec4534b2a4174fddde314b38d30d69093#bc174a7ec4534b2a4174fddde314b38d30d69093"
    ));
}

#[test]
fn release_memory_probe_covers_every_supported_desktop_platform() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs"))
        .expect("desktop library source should be readable");
    for platform_probe in [
        "parse_linux_resident_bytes",
        "MACH_TASK_BASIC_INFO",
        "K32GetProcessMemoryInfo",
    ] {
        assert!(
            source.contains(platform_probe),
            "release memory gate must retain the {platform_probe} platform probe"
        );
    }
    assert!(source.contains("resident_memory_probe_reports_the_current_process"));
    assert!(source.contains("mod resident_memory"));
    assert!(!source.contains("#[cfg(test)]\nmod resident_memory"));

    let manifest = manifest();
    let windows = &manifest["target"]["cfg(target_os = \"windows\")"];
    let windows_sys = &windows["dependencies"]["windows-sys"];
    assert_eq!(windows_sys["version"].as_str(), Some("0.61"));
    let features = windows_sys["features"]
        .as_array()
        .expect("windows-sys features should be explicit")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        features,
        BTreeSet::from([
            "Win32_Foundation",
            "Win32_System_ProcessStatus",
            "Win32_System_Threading",
        ])
    );
}

#[test]
fn external_desktop_performance_gates_are_cross_platform_and_fail_closed() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve");
    let bash_native = fs::read_to_string(root.join("scripts/desktop-native-perf-gate.sh"))
        .expect("bash native gate should be readable");
    let powershell_native = fs::read_to_string(root.join("scripts/desktop-native-perf-gate.ps1"))
        .expect("PowerShell native gate should be readable");
    let powershell_headless = fs::read_to_string(root.join("scripts/desktop-perf-gate.ps1"))
        .expect("PowerShell headless gate should be readable");
    let external_report =
        fs::read_to_string(root.join("scripts/desktop-click-to-photon-report.py"))
            .expect("external click-to-photon report should be readable");

    for gate in [&bash_native, &powershell_native] {
        for contract in [
            "EVO_DESKTOP_MARKDOWN_TRACE",
            "production_markdown_completion_samples",
            "native_rss_steady_growth_bytes",
            "native_rss_absolute_budget_bytes",
            "native_rss_steady_budget_bytes",
        ] {
            assert!(
                gate.contains(contract),
                "native gate must retain {contract}"
            );
        }
    }
    assert!(powershell_headless.contains("desktop_release_"));
    assert!(powershell_headless.contains("desktop_release_gpui_"));
    assert!(external_report.contains("sample_id"));
    assert!(external_report.contains("latency_us"));
    assert!(external_report.contains("paired_app_log"));
    assert!(external_report.contains("p95_budget_us"));
}

#[test]
fn desktop_public_api_is_the_application_boundary_not_an_adapter_sdk() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let library = fs::read_to_string(manifest_dir.join("src/lib.rs"))
        .expect("desktop library root should be readable");
    let binary = fs::read_to_string(manifest_dir.join("src/main.rs"))
        .expect("desktop binary entrypoint should be readable");
    let release_api_script = fs::read_to_string(
        manifest_dir
            .join("../..")
            .join("scripts/release-api-snapshots.sh"),
    )
    .expect("release API snapshot script should be readable");

    assert!(library.contains("pub struct DesktopApplicationOptions"));
    assert!(library.contains("pub fn run(options: DesktopApplicationOptions)"));
    assert!(
        !library.contains("pub mod "),
        "desktop implementation modules must not form a public adapter SDK"
    );
    for implementation_module in [
        "conversation",
        "preferences",
        "projection",
        "runtime",
        "shell",
        "command_ledger",
        "actions",
    ] {
        assert!(library.contains(&format!("mod {implementation_module};")));
    }
    assert_eq!(
        binary.matches("desktop::").count(),
        2,
        "the binary must remain a thin options-plus-entrypoint adapter"
    );
    assert!(
        release_api_script.contains("coding-agent desktop"),
        "the final release API inventory must freeze the desktop application boundary"
    );
}

#[test]
fn desktop_keyboard_actions_are_typed_modal_semantic_and_idle_static() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let actions = fs::read_to_string(manifest_dir.join("src/actions.rs"))
        .expect("desktop action owner should be readable");
    let shell = fs::read_to_string(manifest_dir.join("src/app/native_shell.rs"))
        .expect("desktop native shell should be readable");
    let native_ui = [
        "composer_pane.rs",
        "conversation_header.rs",
        "conversation_pane.rs",
        "inspector_pane.rs",
        "overlay_host.rs",
        "sessions_pane.rs",
        "status_bar.rs",
    ]
    .into_iter()
    .map(|name| {
        fs::read_to_string(manifest_dir.join("src/app/native_shell").join(name)).unwrap_or_else(
            |error| panic!("desktop native UI owner {name} should be readable: {error}"),
        )
    })
    .collect::<Vec<_>>()
    .join("\n");
    let runtime = fs::read_to_string(manifest_dir.join("src/runtime.rs"))
        .expect("desktop runtime should be readable");

    assert!(actions.contains("actions!(\n    desktop"));
    assert!(actions.contains("enum DesktopPaletteCommand"));
    assert!(actions.contains("PALETTE_ENTRIES"));
    assert!(actions.contains("ROOT_KEY_CONTEXT"));
    assert!(actions.contains("PALETTE_KEY_CONTEXT"));
    assert!(actions.contains("AUTHORIZATION_KEY_CONTEXT"));
    assert!(actions.contains("NARROW_SESSIONS_KEY_CONTEXT"));
    assert!(actions.contains("NARROW_INSPECTOR_KEY_CONTEXT"));
    assert!(actions.contains("ToggleInspectorPanel"));
    assert!(actions.contains("Toggle Inspector"));
    assert!(!actions.contains("slash_command"));
    assert!(!actions.contains("rpc"));

    assert!(shell.contains("fn execute_palette_command("));
    assert!(shell.contains("fn on_escape_hierarchy("));
    assert!(shell.contains("fn root_action_blocked_by_overlay("));
    assert!(shell.contains("fn reconcile_authorization_overlay("));
    assert!(shell.contains("secondary: true"));
    assert!(shell.contains("fn submit_primary_composer("));
    assert!(shell.contains(".key_context(actions::ROOT_KEY_CONTEXT)"));
    assert!(native_ui.contains(".key_context(actions::PALETTE_KEY_CONTEXT)"));
    assert!(native_ui.contains(".key_context(actions::AUTHORIZATION_KEY_CONTEXT)"));
    assert!(shell.matches(".tooltip(").count() + native_ui.matches(".tooltip(").count() >= 20);
    assert!(native_ui.contains("motion reduced"));
    assert!(native_ui.contains("motion static"));
    assert!(
        !shell.contains("Timer::after")
            && !shell.contains("Animation")
            && !native_ui.contains("Timer::after")
            && !native_ui.contains("Animation"),
        "native shell must not run an idle presentation timer or ambient animation"
    );

    assert!(runtime.contains("DesktopRuntimeCommand::ListSessions"));
    assert!(runtime.contains("self.context.list_sessions()?"));
    assert!(runtime.contains("MAX_DESKTOP_SESSION_CATALOG"));
}

#[test]
fn desktop_bootstrap_and_native_shell_have_distinct_module_owners() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = fs::read_to_string(manifest_dir.join("src/app.rs"))
        .expect("desktop bootstrap owner should be readable");
    let shell = fs::read_to_string(manifest_dir.join("src/app/native_shell.rs"))
        .expect("desktop native shell owner should be readable");

    assert!(bootstrap.contains("mod native_shell;"));
    assert!(bootstrap.contains("application().run"));
    assert!(bootstrap.contains("DesktopRuntimeBridge::spawn"));
    assert!(!bootstrap.contains("impl Render for NativeShell"));
    assert!(shell.contains("impl Render for NativeShell"));
    assert!(shell.contains("fn submit_composer"));
    assert!(!shell.contains("application().run"));
    assert!(!shell.contains("DesktopRuntimeBridge::spawn"));
}

#[test]
fn native_shell_controllers_keep_update_command_and_conversation_ownership_separate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let shell = fs::read_to_string(root.join("native_shell.rs"))
        .expect("native shell composition owner should be readable");
    let controller_root = root.join("native_shell");
    let update = fs::read_to_string(controller_root.join("update.rs"))
        .expect("runtime update controller should be readable");
    let commands = fs::read_to_string(controller_root.join("commands.rs"))
        .expect("typed command controller should be readable");
    let conversation = fs::read_to_string(controller_root.join("conversation_controller.rs"))
        .expect("conversation controller should be readable");

    for module in ["commands", "conversation_controller", "update"] {
        assert!(shell.contains(&format!("mod {module};")));
    }
    assert!(update.contains("struct ProjectionDirtyRouting"));
    assert!(update.contains("fn inspector_projection_immediate_dirty"));
    assert!(!shell.contains("fn inspector_projection_immediate_dirty"));

    assert!(commands.contains("struct ProjectionCommandCompletions"));
    assert!(commands.contains("fn reconcile_direct_update"));
    assert!(commands.contains("DesktopRuntimeUpdate::FileReviewed"));
    assert!(!shell.contains("DesktopRuntimeUpdate::FileReviewed"));

    for algorithm in [
        "fn row_target_height",
        "fn submit_row_measurement",
        "fn compensate_scroll_top_for_single_row_height",
        "event = \"scroll_anchor_compensate\"",
    ] {
        assert!(
            conversation.contains(algorithm),
            "conversation controller must own {algorithm}"
        );
        assert!(
            !shell.contains(algorithm),
            "native shell composition must not own {algorithm}"
        );
    }
}

#[test]
fn desktop_projection_composes_the_product_reducer_without_shadow_classifiers() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/projection.rs");
    let source = fs::read_to_string(path).expect("desktop projection should be readable");
    let runtime_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime.rs");
    let runtime = fs::read_to_string(runtime_path).expect("desktop runtime should be readable");

    assert!(
        source.contains("CodingAgentClientProjection"),
        "desktop projection must compose the stable product reducer"
    );
    assert!(
        runtime.contains(
            "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
        ),
        "desktop projection must consume the product-owned cross-adapter fixture"
    );
    assert!(
        runtime.contains("shared_cross_adapter_fixture_matches_desktop_product_state_exactly"),
        "desktop projection must compare its complete product state with the shared reducer"
    );
    for forbidden in [
        "fn apply_operation(",
        "fn apply_message(",
        "fn apply_tool(",
        "fn apply_authorization(",
        "fn apply_diagnostic(",
        "fn apply_recovery(",
        "fn apply_change(",
        "fn apply_delegation(",
        "fn apply_usage(",
        "pending_mutations",
        "fn validate_capability_generation(",
        "fn inferred_operation_kind(",
    ] {
        assert!(
            !source.contains(forbidden),
            "desktop projection must not reintroduce product classifier `{forbidden}`"
        );
    }
}

#[test]
fn desktop_uses_the_product_owned_prepared_submission_without_manual_choreography() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime.rs");
    let source = fs::read_to_string(path).expect("desktop runtime should be readable");
    let production = source.as_str();

    assert!(production.contains("connection.prepare_client_submission("));
    assert!(production.contains("let result = submission"));
    assert!(production.contains(".run(&mut session)"));
    for forbidden in [
        "connection.set_prompt_draft(",
        "connection.prepare_submission(",
        "session.run(operation)",
    ] {
        assert!(
            !production.contains(forbidden),
            "desktop must not rebuild product submission choreography: {forbidden}"
        );
    }
}

#[test]
fn desktop_metadata_deliveries_cannot_hydrate_or_replace_the_transcript() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime.rs");
    let source = fs::read_to_string(path).expect("desktop runtime should be readable");
    let metadata_snapshot = source
        .split("fn metadata_snapshot")
        .nth(1)
        .and_then(|tail| tail.split("fn snapshot").next())
        .expect("metadata snapshot owner should remain distinct from full hydration");

    assert!(source.contains("pub struct DesktopRuntimeMetadataSnapshot"));
    assert!(source.contains("pub struct DesktopRuntimeRecoverySnapshot"));
    assert!(source.contains("pub struct DesktopRuntimeHydratedSnapshot"));
    assert!(source.contains("pub enum DesktopRuntimeResyncSnapshot"));
    assert!(
        !source.contains("DesktopRuntimeSnapshot"),
        "the ambiguous optional full-snapshot delivery must not return"
    );
    assert!(source.contains("Reloaded {\n        command_id: u64,\n        metadata:"));
    assert!(source.contains("SelectionChanged {\n        command_id: u64,"));
    assert!(
        source.contains("metadata: DesktopRuntimeMetadataSnapshot"),
        "metadata-only updates must use the narrow delivery type"
    );
    let active_prompt = source
        .split("struct ActivePrompt")
        .nth(1)
        .and_then(|tail| tail.split("enum ActiveSignal").next())
        .expect("active prompt owner should remain explicit");
    assert!(
        !active_prompt.contains("transcript:"),
        "active prompt must not retain or clone a complete transcript baseline"
    );
    assert!(
        source.contains("DesktopRuntimeResyncSnapshot::Metadata("),
        "active resync must preserve the existing transcript through a narrow metadata delivery"
    );
    assert!(
        !metadata_snapshot.contains("transcript_snapshot")
            && !metadata_snapshot.contains("recovery_pending"),
        "metadata snapshot construction must not read durable transcript or recovery payloads"
    );
    for command_path in [
        "reload\n                    .and_then(|()| state.metadata_snapshot())",
        ".and_then(|()| state.metadata_snapshot())\n                .map(|metadata| DesktopRuntimeUpdate::SelectionChanged",
        "self.metadata_snapshot()",
    ] {
        assert!(
            source.contains(command_path),
            "metadata command must use the narrow snapshot path: {command_path}"
        );
    }

    let recovery_snapshot = source
        .split("fn recovery_snapshot")
        .nth(1)
        .and_then(|tail| tail.split("fn retry_recovery").next())
        .expect("recovery snapshot owner should remain distinct from full hydration");
    assert!(
        !recovery_snapshot.contains("transcript_snapshot"),
        "recovery replacement must refresh pending facts without hydrating the transcript"
    );
    assert!(recovery_snapshot.contains("recovery_pending"));

    let projection_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/projection.rs");
    let projection =
        fs::read_to_string(projection_path).expect("desktop projection should be readable");
    let metadata_replacement = projection
        .split("fn replace_metadata_snapshot")
        .nth(1)
        .and_then(|tail| tail.split("fn require_resync").next())
        .expect("metadata replacement should have a dedicated projection owner");
    assert!(!metadata_replacement.contains("replace_transcript"));
    assert!(!metadata_replacement.contains("ConversationProjection::hydrate"));
    let recovery_replacement = projection
        .split("fn replace_recovery_snapshot")
        .nth(1)
        .and_then(|tail| tail.split("fn require_resync").next())
        .expect("recovery replacement should have a dedicated projection owner");
    assert!(!recovery_replacement.contains("replace_transcript"));
    assert!(!recovery_replacement.contains("ConversationProjection::hydrate"));
}

#[test]
fn desktop_runtime_delivery_awaits_events_without_an_idle_poll_loop() {
    let runtime_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime.rs");
    let runtime = fs::read_to_string(runtime_path).expect("desktop runtime should be readable");
    let shell_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/app/native_shell.rs");
    let shell = fs::read_to_string(shell_path).expect("desktop native shell should be readable");

    assert!(runtime.contains("pub struct DesktopRuntimeCommandHandle"));
    assert!(runtime.contains("pub struct DesktopRuntimeEventStream"));
    assert!(runtime.contains("pub struct DesktopRuntimeShutdownGuard"));
    assert!(runtime.contains("pub fn into_parts("));
    assert!(runtime.contains("pub async fn next_update(&mut self)"));
    assert!(shell.contains("runtime.into_parts()"));
    assert!(shell.contains("while let Some(updates) = runtime_events.next_update_batch().await"));
    assert!(shell.contains("runtime_shutdown.shutdown(&mut runtime_events).await"));
    assert!(shell.contains("runtime: Option<DesktopRuntimeCommandHandle>"));
    assert!(
        !shell.contains("RUNTIME_POLL_INTERVAL"),
        "the native shell must not wake periodically while runtime delivery is idle"
    );
    assert!(
        !shell.contains("DesktopRuntimeBridge::try_next_update"),
        "GPUI must await the event stream instead of scanning runtime queues"
    );
    assert!(runtime.contains("STREAMING_DELIVERY_COALESCE_WINDOW"));
    assert!(runtime.contains("if !is_streaming_data_update(&first)"));
    assert!(
        runtime.contains("let immediate = !is_streaming_data_update(&update)"),
        "priority/control/recovery/terminal updates must interrupt data coalescing"
    );
}

#[test]
fn desktop_pending_commands_use_one_bounded_checked_typed_ledger() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ledger = fs::read_to_string(manifest_dir.join("src/command_ledger.rs"))
        .expect("desktop command ledger should be readable");
    let shell = fs::read_to_string(manifest_dir.join("src/app/native_shell.rs"))
        .expect("desktop native shell should be readable");
    let commands = fs::read_to_string(manifest_dir.join("src/app/native_shell/commands.rs"))
        .expect("desktop command controller should be readable");

    assert!(ledger.contains("pub(crate) const MAX_PENDING_DESKTOP_COMMANDS: usize = 32"));
    assert!(ledger.contains("pub(crate) enum DesktopCommandIntent"));
    assert!(ledger.contains("checked_add(1)"));
    assert!(!ledger.contains("saturating_add"));
    assert!(shell.contains("command_ledger: DesktopCommandLedger"));
    assert!(commands.contains("shell.command_ledger.reserve(intent)"));
    for obsolete_pending_field in [
        "next_command_id:",
        "pending_abort_command",
        "pending_reload_command",
        "pending_selection_command",
        "pending_authorization_command",
        "pending_recovery_command",
    ] {
        assert!(
            !shell.contains(obsolete_pending_field),
            "native shell must use the bounded typed command ledger, not {obsolete_pending_field}"
        );
    }
}

#[test]
fn desktop_file_review_uses_product_authority_and_argument_safe_adapter_bounds() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let review = fs::read_to_string(manifest_dir.join("src/file_review.rs"))
        .expect("desktop file review owner should be readable");
    let runtime = fs::read_to_string(manifest_dir.join("src/runtime.rs"))
        .expect("desktop runtime owner should be readable");
    let shell = fs::read_to_string(manifest_dir.join("src/app/native_shell.rs"))
        .expect("desktop native shell should be readable");
    let inspector = fs::read_to_string(manifest_dir.join("src/app/native_shell/inspector_pane.rs"))
        .expect("desktop inspector pane should be readable");

    for bound in [
        "MAX_VISIBLE_FILE_CHANGES: usize = 64",
        "MAX_REVIEW_ROWS: usize = 480",
        "MAX_REVIEW_LINE_BYTES: usize = 2 * 1024",
        "MAX_REVIEW_RENDER_BYTES: usize = 160 * 1024",
        "MAX_REVIEW_CLIPBOARD_BYTES: usize = 128 * 1024",
    ] {
        assert!(review.contains(bound), "file review omitted bound {bound}");
    }
    assert!(runtime.contains(".review_changed_file(request)"));
    assert!(
        runtime.contains("session.revalidate_external_editor_target(&target)"),
        "external launch must revalidate the opaque product target immediately before spawn"
    );
    assert!(review.contains("args.push(validated_path.as_os_str().to_owned())"));
    assert!(review.contains("Command::new(invocation.program)"));
    assert!(review.contains(".args(invocation.args)"));
    assert!(!review.contains("Command::new(\"sh\")"));
    assert!(!review.contains(".arg(\"-c\")"));
    assert!(!shell.contains("std::fs"));
    assert!(!shell.contains("std::process::Command"));
    assert!(!inspector.contains("std::fs"));
    assert!(!inspector.contains("std::process::Command"));
    assert!(inspector.contains(".take(MAX_VISIBLE_FILE_CHANGES)"));
    assert!(shell.contains("DesktopCommandIntent::FileReview"));
    assert!(shell.contains("DesktopCommandIntent::ExternalEditor"));
}
