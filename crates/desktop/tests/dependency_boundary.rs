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

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
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
    // Icons must come from one bundled, accessible set rather than hand-drawn
    // SVGs, and the asset crate must track the component revision exactly so an
    // `IconName` can never outrun the assets that back it.
    assert_eq!(
        dependencies["gpui-component-assets"]["rev"].as_str(),
        dependencies["gpui-component"]["rev"].as_str(),
        "bundled icon assets must be pinned to the component revision"
    );
    assert_eq!(
        dependencies["gpui-component-assets"]["git"].as_str(),
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
    let bash_headless = fs::read_to_string(root.join("scripts/desktop-perf-gate.sh"))
        .expect("bash headless gate should be readable");
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
    let release_tests = [
        "conversation::model::tests::desktop_release_empty_conversation_baseline",
        "conversation::model::tests::desktop_release_ten_mib_interaction_baseline",
        "conversation::model::tests::desktop_release_scale_content_and_streaming_matrix",
        "app::native_shell::tests::desktop_release_gpui_headless_frame_and_input_replay",
        "app::native_shell::tests::desktop_release_gpui_markdown_parser_matrix",
    ];
    for gate in [&bash_headless, &powershell_headless] {
        for release_test in release_tests {
            assert!(
                gate.contains(release_test),
                "headless gate must run {release_test} by its stable full path"
            );
        }
        assert!(
            gate.contains("--exact"),
            "headless gate must not admit another test through a prefix filter"
        );
        assert!(
            gate.contains("running 1 test"),
            "headless gate must fail when Cargo silently runs zero tests"
        );
    }
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
    let runtime_driver = fs::read_to_string(manifest_dir.join("src/runtime/driver.rs"))
        .expect("desktop runtime driver should be readable");
    let runtime_protocol = fs::read_to_string(manifest_dir.join("src/runtime/protocol.rs"))
        .expect("desktop runtime protocol should be readable");

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
    assert!(native_ui.contains("secondary: true"));
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

    assert!(runtime_protocol.contains("ListSessions"));
    assert!(runtime_driver.contains("self.context.list_sessions()?"));
    assert!(runtime_protocol.contains("MAX_DESKTOP_SESSION_CATALOG"));
}

#[test]
fn desktop_bootstrap_and_native_shell_have_distinct_module_owners() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bootstrap = fs::read_to_string(manifest_dir.join("src/app.rs"))
        .expect("desktop bootstrap owner should be readable");
    let shell = fs::read_to_string(manifest_dir.join("src/app/native_shell.rs"))
        .expect("desktop native shell owner should be readable");

    assert!(bootstrap.contains("mod native_shell;"));
    assert!(bootstrap.contains("application()"));
    assert!(bootstrap.contains(".run(move |cx: &mut App|"));
    assert!(bootstrap.contains("DesktopRuntimeBridge::spawn"));
    // Icon assets are an application-startup concern: registering the source
    // anywhere else would leave icon-only controls rendering blank.
    assert!(bootstrap.contains(".with_assets(gpui_component_assets::Assets)"));
    assert!(!bootstrap.contains("impl Render for NativeShell"));
    assert!(shell.contains("impl Render for NativeShell"));
    assert!(shell.contains("fn submit_composer"));
    assert!(!shell.contains("application()"));
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
    let sessions = fs::read_to_string(controller_root.join("sessions_pane.rs"))
        .expect("sessions pane should be readable");
    let session_controller = fs::read_to_string(controller_root.join("session_controller.rs"))
        .expect("session controller should be readable");
    let composer = fs::read_to_string(controller_root.join("composer_pane.rs"))
        .expect("composer pane should be readable");
    let inspector = fs::read_to_string(controller_root.join("inspector_pane.rs"))
        .expect("inspector pane should be readable");
    let overlay = fs::read_to_string(controller_root.join("overlay_host.rs"))
        .expect("overlay host should be readable");

    for module in [
        "commands",
        "conversation_controller",
        "session_controller",
        "update",
    ] {
        assert!(shell.contains(&format!("mod {module};")));
    }
    assert!(update.contains("struct ProjectionDirtyRouting"));
    assert!(update.contains("fn inspector_projection_immediate_dirty"));
    assert!(!shell.contains("fn inspector_projection_immediate_dirty"));

    assert!(commands.contains("struct ProjectionCommandCompletions"));
    assert!(commands.contains("fn reconcile_direct_update"));
    assert!(commands.contains("DesktopRuntimeUpdate::FileReviewed"));
    assert!(!shell.contains("DesktopRuntimeUpdate::FileReviewed"));
    assert!(sessions.contains("struct SessionsPaneViewModel"));
    assert!(sessions.contains("search_input: gpui::Entity<InputState>"));
    assert!(!sessions.contains("WeakEntity<NativeShell>"));
    assert!(!sessions.contains("owner.read(cx)"));
    assert!(session_controller.contains("struct SessionController"));
    assert!(session_controller.contains("refresh_deadline: Option<Instant>"));
    assert!(session_controller.contains("fn schedule_session_catalog_refresh"));
    assert!(!shell.contains("session_catalog_refresh_deadline"));

    let root_input_field = ["composer_", "input:"].concat();
    let root_latency_field = ["composer_", "input_latency:"].concat();
    let weak_root_owner = ["WeakEntity", "<NativeShell>"].concat();
    assert!(composer.contains("struct ComposerPaneViewModel"));
    assert!(composer.contains("input: gpui::Entity<InputState>"));
    assert!(composer.contains("focus: FocusHandle"));
    assert!(composer.contains("latency: InputRenderLatencyProbe"));
    assert!(composer.contains("ComposerPaneEvent::InputChanged"));
    assert!(composer.contains("ComposerPaneEvent::Focused"));
    assert!(composer.contains("ComposerPaneEvent::SubmitPrimary"));
    assert!(!composer.contains(&weak_root_owner));
    assert!(!composer.contains("owner.read(cx)"));
    assert!(!composer.contains("DesktopProjection"));
    assert!(!shell.contains(&root_input_field));
    assert!(!shell.contains(&root_latency_field));
    assert!(shell.contains("fn composer_pane_view_model(&self) -> ComposerPaneViewModel"));
    assert!(shell.contains("composer: ComposerState"));
    assert!(shell.contains("composer_session_drafts: HashMap<String, String>"));
    assert!(shell.contains("composer_running_modes: HashMap<String, ComposerRunningMode>"));

    for (name, source) in [("inspector", &inspector), ("overlay", &overlay)] {
        assert!(
            !source.contains(&weak_root_owner),
            "{name} must not retain a NativeShell back-reference"
        );
        assert!(
            !source.contains("owner.read(cx)"),
            "{name} must render only from its ViewModel"
        );
        assert!(
            !source.contains("DesktopProjection"),
            "{name} must not expose the full projection"
        );
        assert!(
            !source.contains("command_ledger"),
            "{name} must receive only derived pending state"
        );
    }
    assert!(inspector.contains("struct InspectorPaneViewModel"));
    assert!(inspector.contains("view_model: Option<InspectorPaneViewModel>"));
    assert!(inspector.contains("file_review: Arc<DesktopFileReviewState>"));
    assert!(inspector.contains("identity: DesktopRecoveryIdentity"));
    assert!(!inspector.contains("preferences."));
    assert!(shell.contains("fn inspector_pane_view_model(&self) -> InspectorPaneViewModel"));
    assert!(shell.contains("file_review: Arc<DesktopFileReviewState>"));
    assert!(shell.contains("inspector_telemetry_refresh_deadline: Option<Instant>"));
    assert!(shell.contains("inspector_session_sections: HashMap<String, InspectorSection>"));
    assert!(overlay.contains("struct OverlayViewModel"));
    assert!(overlay.contains("view_model: Option<OverlayViewModel>"));
    assert!(overlay.contains("request: ToolAuthorizationRequest"));
    assert!(!overlay.contains("session_controller"));
    assert!(shell.contains("fn overlay_view_model(&self) -> OverlayViewModel"));
    assert!(shell.contains("active_overlay: Option<DesktopOverlayKind>"));

    // Ownership assertions target production code: the shell's own test module
    // still constructs conversation fixtures directly.
    let shell_production = shell
        .split_once("\n#[cfg(test)]\nmod tests {")
        .map_or(shell.as_str(), |(production, _)| production);

    for algorithm in [
        "fn row_target_height",
        "fn submit_row_measurement",
        "fn compensate_scroll_top_for_single_row_height",
        "event = \"scroll_anchor_compensate\"",
        "fn rebuild_rows",
        "fn rebuild_live_rows",
        "fn update_rows_by_sequence",
        "fn upsert_render_row",
        "fn live_rows_match",
        "fn prepare_rows",
        "fn width_for_render",
        "fn reconcile_session_view",
        "fn apply_delta",
        "ConversationRowRenderSource",
        "event = \"session_scroll_restore\"",
    ] {
        assert!(
            conversation.contains(algorithm),
            "conversation controller must own {algorithm}"
        );
        assert!(
            !shell_production.contains(algorithm),
            "native shell composition must not own {algorithm}"
        );
    }

    // The conversation controller is the sole owner of transcript cache,
    // layout, viewport and dirty-sequence state; the root only supplies a
    // bounded projection source and consumes a view model.
    for state in [
        "viewport: ConversationViewport",
        "layout: ConversationRowLayoutState",
        "render_cache: ConversationRowRenderCache",
        "render_dirty_sequences: VecDeque<u64>",
        "render_sequence_overflow: bool",
        "render_width_bucket: Option<u32>",
        "height_refresh_deadline: Option<Instant>",
        "expanded_details: HashSet<String>",
        "session_views: HashMap<String, ConversationSessionViewState>",
    ] {
        assert!(
            conversation.contains(state),
            "conversation controller must own {state}"
        );
        assert!(
            !shell_production.contains(state),
            "native shell composition must not own {state}"
        );
    }
    assert!(conversation.contains("struct ConversationSource<'a>"));
    assert!(shell.contains("conversation_controller: ConversationController"));
    assert!(shell.contains("fn conversation_pane_view_model(&self) -> ConversationPaneViewModel"));
    for back_reference in [
        &weak_root_owner,
        &"&mut NativeShell".to_owned(),
        &"Context<NativeShell>".to_owned(),
        &"super::NativeShell".to_owned(),
    ] {
        assert!(
            !conversation.contains(back_reference.as_str()),
            "conversation controller must not reach back into the composition root via \
             {back_reference}"
        );
    }
}

#[test]
fn conversation_presentation_modules_have_stable_acyclic_owners() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(
        !source_root.join("conversation.rs").exists(),
        "the legacy conversation.rs owner must be replaced by conversation/mod.rs"
    );
    let conversation_root = source_root.join("conversation");
    let module = fs::read_to_string(conversation_root.join("mod.rs"))
        .expect("conversation re-export module should be readable");

    for owner in [
        "composer",
        "copy",
        "layout",
        "markdown",
        "model",
        "render_cache",
        "viewport",
    ] {
        assert!(
            conversation_root.join(format!("{owner}.rs")).is_file(),
            "conversation owner {owner}.rs must exist"
        );
        assert!(
            module.contains(&format!("mod {owner};")),
            "conversation/mod.rs must declare {owner}"
        );
    }
    for stable_surface in [
        "pub use composer::",
        "pub use copy::",
        "pub use layout::",
        "pub use markdown::",
        "pub use model::",
        "pub use render_cache::",
        "pub use viewport::",
    ] {
        assert!(
            module.contains(stable_surface),
            "conversation/mod.rs must retain {stable_surface}"
        );
    }
    assert!(!module.contains("pub struct "));
    assert!(!module.contains("pub enum "));
    assert!(!module.contains("pub fn "));

    let read_owner = |name: &str| {
        fs::read_to_string(conversation_root.join(format!("{name}.rs"))).unwrap_or_else(|error| {
            panic!("conversation owner {name}.rs should be readable: {error}")
        })
    };
    let composer = read_owner("composer");
    let copy = read_owner("copy");
    let layout = read_owner("layout");
    let markdown = read_owner("markdown");
    let model = read_owner("model");
    let render_cache = read_owner("render_cache");
    let viewport = read_owner("viewport");
    let composer = production_source(&composer);
    let copy = production_source(&copy);
    let layout = production_source(&layout);
    let markdown = production_source(&markdown);
    let model = production_source(&model);
    let render_cache = production_source(&render_cache);
    let viewport = production_source(&viewport);

    // The production dependency graph is a DAG:
    // model -> copy; composer -> model/copy; render_cache -> model/markdown;
    // layout -> model/render_cache; viewport -> model.
    assert!(model.contains("super::copy"));
    for forbidden in ["composer", "layout", "markdown", "render_cache", "viewport"] {
        assert!(
            !model.contains(&format!("super::{forbidden}")),
            "model must not depend on downstream owner {forbidden}"
        );
    }
    assert!(!copy.contains("super::model"));
    assert!(!markdown.contains("super::model"));
    assert!(composer.contains("super::copy"));
    assert!(composer.contains("super::model"));
    assert!(render_cache.contains("super::markdown"));
    assert!(render_cache.contains("super::model"));
    assert!(!render_cache.contains("super::layout"));
    assert!(layout.contains("model::ConversationItemKey"));
    assert!(layout.contains("render_cache::StreamingTextPhase"));
    assert!(!layout.contains("super::viewport"));
    assert!(viewport.contains("super::model"));
    assert!(!viewport.contains("super::layout"));
    assert!(!viewport.contains("super::render_cache"));
}

#[test]
fn desktop_projection_composes_the_product_reducer_without_shadow_classifiers() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/projection.rs");
    let source = fs::read_to_string(path).expect("desktop projection should be readable");
    let runtime_tests_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime/tests.rs");
    let runtime_tests =
        fs::read_to_string(runtime_tests_path).expect("desktop runtime tests should be readable");

    assert!(
        source.contains("CodingAgentClientProjection"),
        "desktop projection must compose the stable product reducer"
    );
    assert!(
        runtime_tests.contains(
            "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
        ),
        "desktop projection must consume the product-owned cross-adapter fixture"
    );
    assert!(
        runtime_tests
            .contains("shared_cross_adapter_fixture_matches_desktop_product_state_exactly"),
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime/driver.rs");
    let source = fs::read_to_string(path).expect("desktop runtime driver should be readable");
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
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let driver = fs::read_to_string(manifest_dir.join("src/runtime/driver.rs"))
        .expect("desktop runtime driver should be readable");
    let dispatch = fs::read_to_string(manifest_dir.join("src/runtime/dispatch.rs"))
        .expect("desktop runtime dispatch should be readable");
    let protocol = fs::read_to_string(manifest_dir.join("src/runtime/protocol.rs"))
        .expect("desktop runtime protocol should be readable");
    let metadata_snapshot = driver
        .split("fn metadata_snapshot")
        .nth(1)
        .and_then(|tail| tail.split("fn snapshot").next())
        .expect("metadata snapshot owner should remain distinct from full hydration");

    assert!(protocol.contains("pub struct DesktopRuntimeMetadataSnapshot"));
    assert!(protocol.contains("pub struct DesktopRuntimeRecoverySnapshot"));
    assert!(protocol.contains("pub struct DesktopRuntimeHydratedSnapshot"));
    assert!(protocol.contains("pub enum DesktopRuntimeResyncSnapshot"));
    assert!(
        !driver.contains("DesktopRuntimeSnapshot")
            && !dispatch.contains("DesktopRuntimeSnapshot")
            && !protocol.contains("DesktopRuntimeSnapshot"),
        "the ambiguous optional full-snapshot delivery must not return"
    );
    assert!(protocol.contains("Reloaded {\n        command_id: u64,\n        metadata:"));
    assert!(protocol.contains("SelectionChanged {\n        command_id: u64,"));
    assert!(
        protocol.contains("metadata: DesktopRuntimeMetadataSnapshot"),
        "metadata-only updates must use the narrow delivery type"
    );
    let active_prompt = driver
        .split("struct ActivePrompt")
        .nth(1)
        .and_then(|tail| tail.split("enum ActiveSignal").next())
        .expect("active prompt owner should remain explicit");
    assert!(
        !active_prompt.contains("transcript:"),
        "active prompt must not retain or clone a complete transcript baseline"
    );
    assert!(
        dispatch.contains("DesktopRuntimeResyncSnapshot::Metadata("),
        "active resync must preserve the existing transcript through a narrow metadata delivery"
    );
    assert!(
        !metadata_snapshot.contains("transcript_snapshot")
            && !metadata_snapshot.contains("recovery_pending"),
        "metadata snapshot construction must not read durable transcript or recovery payloads"
    );
    for command_path in [
        "reload\n                .and_then(|()| state.metadata_snapshot())",
        ".and_then(|()| state.metadata_snapshot())\n            .map(|metadata| DesktopRuntimeUpdate::SelectionChanged",
    ] {
        assert!(
            dispatch.contains(command_path),
            "metadata command must use the narrow snapshot path: {command_path}"
        );
    }
    assert!(driver.contains("self.metadata_snapshot()"));

    let recovery_snapshot = driver
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
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime = fs::read_to_string(manifest_dir.join("src/runtime.rs"))
        .expect("desktop runtime should be readable");
    let protocol = fs::read_to_string(manifest_dir.join("src/runtime/protocol.rs"))
        .expect("desktop runtime protocol should be readable");
    let bridge = fs::read_to_string(manifest_dir.join("src/runtime/bridge.rs"))
        .expect("desktop runtime bridge should be readable");
    let dispatch = fs::read_to_string(manifest_dir.join("src/runtime/dispatch.rs"))
        .expect("desktop runtime dispatch should be readable");
    let driver = fs::read_to_string(manifest_dir.join("src/runtime/driver.rs"))
        .expect("desktop runtime driver should be readable");
    let shell_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/app/native_shell.rs");
    let shell = fs::read_to_string(shell_path).expect("desktop native shell should be readable");

    assert!(runtime.contains("mod bridge;"));
    assert!(runtime.contains("mod dispatch;"));
    assert!(runtime.contains("mod driver;"));
    assert!(runtime.contains("mod protocol;"));
    assert!(runtime.contains("pub use bridge::{"));
    assert!(runtime.contains("pub use protocol::{"));
    assert!(runtime.contains("use driver::run_runtime;"));
    assert!(runtime.contains("mod tests;"));
    assert!(!runtime.contains("struct RuntimeState"));
    assert!(!runtime.contains("tokio::select!"));
    assert!(!protocol.contains("tokio::"));
    assert!(!protocol.contains("RuntimeState"));
    assert!(!protocol.contains("run_runtime"));
    assert!(!bridge.contains("struct RuntimeState"));
    assert!(!bridge.contains("CodingAgentSession"));
    assert!(!bridge.contains("CodingAgentClientConnection"));
    assert!(bridge.contains("pub struct DesktopRuntimeCommandHandle"));
    assert!(bridge.contains("pub struct DesktopRuntimeEventStream"));
    assert!(bridge.contains("pub struct DesktopRuntimeShutdownGuard"));
    assert!(bridge.contains("pub fn into_parts("));
    assert!(bridge.contains("pub async fn next_update(&mut self)"));
    assert!(driver.contains("struct RuntimeState"));
    assert!(driver.contains("struct ActivePrompt"));
    assert!(driver.contains("async fn run_runtime("));
    assert!(driver.contains("tokio::select!"));
    assert!(driver.contains("recover_product_event_source("));
    assert!(driver.contains("drain_product_events("));
    assert!(driver.contains("RUNTIME_SHUTDOWN_DEADLINE"));
    assert!(driver.contains("shutdown_deadline_exceeded"));
    assert!(!driver.contains("let result = match command {"));
    assert!(dispatch.contains("async fn dispatch_idle_command("));
    assert!(dispatch.contains("fn dispatch_active_command("));
    assert!(dispatch.matches("let result = match command {").count() == 2);
    for forbidden in [
        "tokio::select!",
        "CodingAgentReconnectDelivery",
        "acknowledge_product_event",
        "drain_product_events",
        "RUNTIME_SHUTDOWN_DEADLINE",
        "shutdown_deadline_exceeded",
    ] {
        assert!(
            !dispatch.contains(forbidden),
            "command dispatch must not own driver lifecycle behavior: {forbidden}"
        );
    }
    let publish_then_ack = driver
        .split("if !publish_product_event(")
        .nth(1)
        .and_then(|tail| tail.split("active_prompt.last_forwarded_sequence").next())
        .expect("driver should publish and acknowledge before advancing its cursor");
    assert!(publish_then_ack.contains("acknowledge_product_event("));
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
    assert!(bridge.contains("STREAMING_DELIVERY_COALESCE_WINDOW"));
    assert!(bridge.contains("if !is_streaming_data_update(&first)"));
    assert!(
        bridge.contains("let immediate = !is_streaming_data_update(&update)"),
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
    let runtime_driver = fs::read_to_string(manifest_dir.join("src/runtime/driver.rs"))
        .expect("desktop runtime driver should be readable");
    let runtime_dispatch = fs::read_to_string(manifest_dir.join("src/runtime/dispatch.rs"))
        .expect("desktop runtime dispatch should be readable");
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
    assert!(runtime_dispatch.contains(".review_changed_file(request)"));
    assert!(
        runtime_driver.contains("session.revalidate_external_editor_target(&target)"),
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
    assert!(shell.contains(".take(MAX_VISIBLE_FILE_CHANGES)"));
    assert!(!inspector.contains("MAX_VISIBLE_FILE_CHANGES"));
    assert!(shell.contains("DesktopCommandIntent::FileReview"));
    assert!(shell.contains("DesktopCommandIntent::ExternalEditor"));
}
