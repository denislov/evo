fn session_key(session_id: &str) -> WorkspaceKey {
    WorkspaceKey::session(session_id)
}

fn make_session_workspace(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
) -> SessionWorkspace {
    session_workspace_with_thinking(
        project,
        projection,
        preference_notice,
        DesktopThinkingLevel::Default,
    )
}

fn active_session_id(shell: &NativeShell) -> Option<&str> {
    shell
        .app
        .workspaces
        .active_key()
        .session_id()
        .map(SessionId::as_str)
}

fn insert_session_workspace(
    shell: &mut NativeShell,
    session_id: &str,
    workspace: SessionWorkspace,
) {
    assert!(
        shell
            .app
            .workspaces
            .insert_session(SessionId::from_dto(session_id), workspace)
            .is_none(),
        "test session IDs must be unique"
    );
}

fn workspace_for_session<'a>(
    shell: &'a NativeShell,
    session_id: &str,
) -> Option<&'a SessionWorkspace> {
    shell.app.workspaces.get(&session_key(session_id))
}

fn activate_session(shell: &mut NativeShell, session_id: &str) -> bool {
    shell.app.workspaces.activate(&session_key(session_id))
}

fn set_project_directory_for_test(shell: &mut NativeShell, path: PathBuf) -> bool {
    let owner = shell.app.workspaces.active_key().clone();
    PlatformUpdatePort::set_project_directory(shell, &owner, path)
}

fn apply_picker_result_for_test(
    shell: &mut NativeShell,
    picker: DesktopPickerKind,
    outcome: PlatformOutcome<Vec<PathBuf>>,
    cx: &mut Context<NativeShell>,
) {
    let owner = shell.app.workspaces.active_key().clone();
    let transition = shell
        .connection
        .controller
        .pick_paths(owner, picker)
        .expect("test picker effect identity is available");
    let identity = match transition.effects().first() {
        Some(DesktopEffect::PickPaths { identity, .. }) => identity.clone(),
        _ => panic!("picker request must emit one typed picker effect"),
    };
    shell.dispatch_platform_result(
        PlatformResult::PathsPicked {
            identity,
            picker,
            outcome,
        },
        cx,
    );
}

fn session_workspace_ids(shell: &NativeShell) -> HashSet<String> {
    shell
        .app
        .workspaces
        .iter()
        .filter_map(|(key, _)| key.session_id().map(|id| id.as_str().to_owned()))
        .collect()
}

fn visual_test_snapshot() -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
    visual_test_snapshot_for("desktop-visual-test")
}

fn visual_test_snapshot_for(session_id: &str) -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
    let session_id = session_id.to_owned();
    let stream_id = format!("{session_id}-stream");
    desktop::runtime::DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: std::path::PathBuf::from("/desktop-visual-test"),
            workspace: None,
            global_config_dir: std::path::PathBuf::from("/desktop-visual-test/config"),
            selected_model_id: "test-model".into(),
            default_agent_profile_id: ProfileId::from("default"),
            models: vec![
                CodingAgentModelChoice {
                    id: "test-model".into(),
                    name: "Test Model".into(),
                    provider: "fixture".into(),
                    reasoning: true,
                    thinking_capability: CodingAgentThinkingCapability {
                        supported: true,
                        explicit_levels: vec![
                            CodingAgentThinkingLevel::Minimal,
                            CodingAgentThinkingLevel::Low,
                            CodingAgentThinkingLevel::Medium,
                            CodingAgentThinkingLevel::High,
                            CodingAgentThinkingLevel::XHigh,
                        ],
                        can_disable: true,
                    },
                    supports_text: true,
                    supports_images: true,
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    configured: true,
                    selected: true,
                },
                CodingAgentModelChoice {
                    id: "adjacent-model".into(),
                    name: "Adjacent Model".into(),
                    provider: "fixture".into(),
                    reasoning: false,
                    thinking_capability: CodingAgentThinkingCapability::default(),
                    supports_text: true,
                    supports_images: false,
                    context_window: 80_000,
                    max_output_tokens: 8_000,
                    configured: true,
                    selected: false,
                },
                CodingAgentModelChoice {
                    id: "exact-target-model".into(),
                    name: "Exact Target".into(),
                    provider: "fixture".into(),
                    reasoning: false,
                    thinking_capability: CodingAgentThinkingCapability::default(),
                    supports_text: true,
                    supports_images: false,
                    context_window: 100_000,
                    max_output_tokens: 16_000,
                    configured: true,
                    selected: false,
                },
                CodingAgentModelChoice {
                    id: "image-only-model".into(),
                    name: "Image Only".into(),
                    provider: "fixture".into(),
                    reasoning: false,
                    thinking_capability: CodingAgentThinkingCapability::default(),
                    supports_text: false,
                    supports_images: true,
                    context_window: 32_000,
                    max_output_tokens: 4_000,
                    configured: true,
                    selected: false,
                },
            ],
            profiles: vec![
                CodingAgentProfileChoice {
                    id: ProfileId::from("default"),
                    display_name: "Default".into(),
                    description: Some("General coding work".into()),
                    kind: ProfileKind::Agent,
                    source: ProfileSource::BuiltIn,
                    model_id: None,
                },
                CodingAgentProfileChoice {
                    id: ProfileId::from("exact-reviewer"),
                    display_name: "Exact Reviewer".into(),
                    description: Some("Review changes before completion".into()),
                    kind: ProfileKind::Agent,
                    source: ProfileSource::Project,
                    model_id: Some("exact-target-model".into()),
                },
                CodingAgentProfileChoice {
                    id: ProfileId::from("review-team"),
                    display_name: "Review Team".into(),
                    description: Some("Delegated review team".into()),
                    kind: ProfileKind::Team,
                    source: ProfileSource::Project,
                    model_id: None,
                },
            ],
            resources: CodingAgentResourceSummary {
                skill_names: Vec::new(),
                prompt_template_names: Vec::new(),
                commands: Vec::new(),
                context_files: Vec::new(),
            },
            settings: CodingAgentSettingsSummary {
                default_provider: None,
                default_model: None,
                default_thinking_level: None,
                session_dir: None,
                no_context_files: true,
            },
            diagnostics: Vec::new(),
        },
        session: CodingAgentSnapshot {
            cursor: CodingAgentSnapshotCursor {
                stream_id,
                snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
                last_event_sequence: 0,
                last_session_sequence: 0,
                capability_generation: 0,
            },
            version: UI_SNAPSHOT_PROTOCOL_VERSION,
            session: CodingAgentSessionView {
                session_id: session_id.clone(),
                default_agent_profile_id: ProfileId::from("default"),
            },
            capabilities: CodingAgentCapabilities::idle(false),
            active_operation: None,
            drafts: Vec::new(),
            submitted_operation: None,
            pending_authorizations: Vec::new(),
            context: CodingAgentContextSnapshot::default(),
        },
        transcript: CodingAgentTranscriptSnapshot {
            session_id,
            active_leaf_id: None,
            items: Vec::new(),
        },
        pending_recoveries: Vec::new(),
    }
}

fn visual_test_projection() -> DesktopProjection {
    DesktopProjection::new(visual_test_snapshot())
        .expect("visual test fixture is a valid product projection")
}

fn visual_performance_projection(block_count: usize) -> DesktopProjection {
    let mut snapshot = visual_test_snapshot();
    let payload = "headless frame replay 中文 🙂 ".repeat(8);
    snapshot.transcript.items = (0..block_count)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}: {payload}"),
            started_at: None,
        })
        .collect();
    DesktopProjection::new(snapshot)
        .expect("headless frame replay fixture is a valid product projection")
}

fn clipping_regression_projection() -> DesktopProjection {
    let mut snapshot = visual_test_snapshot();
    let mut text = String::from(
        "# Complete final response\n\n> The tail marker must remain inside the measured row.\n\n",
    );
    for line in 1..=60 {
        text.push_str(&format!(
            "{line}. Layout line {line} — 长中文内容用于验证系统字体回退和换行 🙂 e\u{301}\n"
        ));
    }
    text.push_str(
            "\n- list item one\n- list item two\n\n| column | value |\n|---|---|\n| 中文 | 🙂 |\n\n```rust\nfn tail() {\n    println!(\"visible\");\n}\n```\n\nFINAL TAIL TEXT",
        );
    snapshot
        .transcript
        .items
        .push(CodingAgentSessionTranscriptItem::Assistant {
            id: "clipping-regression-final".into(),
            text,
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        });
    DesktopProjection::new(snapshot)
        .expect("clipping regression fixture is a valid product projection")
}

fn long_integrity_text(label: &str) -> String {
    let mut text = format!("# {label}\n\n> Every final line must remain measurable.\n\n");
    for line in 1..=60 {
        text.push_str(&format!(
            "{line}. {label} line {line} — 中文换行 🙂 e\u{301} {}\n",
            "unbroken-width-probe".repeat(8)
        ));
    }
    text.push_str("\nFINAL TYPE-SPECIFIC TAIL");
    text
}

fn projection_with_last_item(item: CodingAgentSessionTranscriptItem) -> DesktopProjection {
    let mut snapshot = visual_test_snapshot();
    snapshot.transcript.items.push(item);
    DesktopProjection::new(snapshot)
        .expect("message-integrity fixture is a valid product projection")
}

fn projection_with_items(items: Vec<CodingAgentSessionTranscriptItem>) -> DesktopProjection {
    let mut snapshot = visual_test_snapshot();
    snapshot.transcript.items = items;
    DesktopProjection::new(snapshot)
        .expect("multi-item conversation fixture is a valid product projection")
}

fn settle_visual_measurements(cx: &mut gpui::VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(100));
    for _ in 0..4 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
}

fn assert_last_row_matches_card_and_tail(cx: &mut gpui::VisualTestContext, label: &str) {
    let row = cx
        .debug_bounds("conversation-last-row")
        .unwrap_or_else(|| panic!("{label}: final virtual row is mounted"));
    let card = cx
        .debug_bounds("conversation-last-card")
        .unwrap_or_else(|| panic!("{label}: final card is laid out"));
    let tail = cx
        .debug_bounds("conversation-tail-marker")
        .unwrap_or_else(|| panic!("{label}: tail marker is laid out"));
    let composer = cx
        .debug_bounds("desktop-composer-panel")
        .unwrap_or_else(|| panic!("{label}: Composer remains visible"));

    assert!(
        (f32::from(row.size.height)
            - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
            .abs()
            <= 1.,
        "{label}: virtual row must match the actual card: row={row:?}, card={card:?}"
    );
    assert!(
        tail.bottom() <= row.bottom() + px(1.),
        "{label}: tail must remain inside the row: tail={tail:?}, row={row:?}"
    );
    assert!(
        tail.bottom() <= composer.top() + px(1.),
        "{label}: tail must remain above the Composer: tail={tail:?}, composer={composer:?}"
    );
}

fn initialize_visual_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        actions::bind_keys(cx);
        Theme::change(ThemeMode::Dark, None, cx);
    });
}

fn visual_global_skills() -> Arc<[CodingAgentResourceCommand]> {
    Arc::from([CodingAgentResourceCommand {
        name: "review-plan".into(),
        command: "/review-plan".into(),
        description: "Review an implementation plan before coding.".into(),
        kind: CodingAgentResourceCommandKind::Skill,
        model_invocable: true,
    }])
}

fn add_visual_shell(
    cx: &mut TestAppContext,
    runtime: DesktopRuntimeBridge,
    projection: DesktopProjection,
) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
    add_visual_shell_with_preferences(cx, runtime, projection, DesktopPreferences::default())
}

fn visual_preferences_with_inspector() -> DesktopPreferences {
    DesktopPreferences {
        context_panel_visible: true,
        ..DesktopPreferences::default()
    }
}

fn add_visual_shell_with_preferences(
    cx: &mut TestAppContext,
    runtime: DesktopRuntimeBridge,
    projection: DesktopProjection,
    preferences: DesktopPreferences,
) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
    let shell_slot = Rc::new(RefCell::new(None));
    let shell_slot_for_window = Rc::clone(&shell_slot);
    let (_, visual_cx) = cx.add_window_view(move |window, cx| {
        let shell = cx.new(|cx| {
            NativeShell::new(
                NativeShellInit {
                    runtime,
                    workspace: NativeShellWorkspaceInit::Session(Box::new(projection)),
                    projectless_workspace_selection: CodingAgentWorkspaceSelection::projectless(
                        "workspace-native-fixture",
                    ),
                    global_skills: visual_global_skills(),
                    preferences,
                    preference_writer: None,
                    preference_notice: None,
                },
                window,
                cx,
            )
        });
        shell_slot_for_window.replace(Some(shell.clone()));
        gpui_component::Root::new(shell, window, cx)
    });
    let shell = shell_slot
        .borrow_mut()
        .take()
        .expect("visual shell entity was captured");
    (shell, visual_cx)
}

fn add_idle_visual_shell(
    cx: &mut TestAppContext,
) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
    add_idle_visual_shell_with_runtime(cx, DesktopRuntimeBridge::disconnected_for_test())
}

fn add_idle_visual_shell_with_runtime(
    cx: &mut TestAppContext,
    runtime: DesktopRuntimeBridge,
) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
    add_idle_visual_shell_with_preferences(cx, runtime, DesktopPreferences::default())
}

fn add_idle_visual_shell_with_preferences(
    cx: &mut TestAppContext,
    runtime: DesktopRuntimeBridge,
    preferences: DesktopPreferences,
) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
    let shell_slot = Rc::new(RefCell::new(None));
    let shell_slot_for_window = Rc::clone(&shell_slot);
    let mut project = visual_test_snapshot().project;
    project.global_config_dir = std::path::PathBuf::from("/desktop-global");
    project.cwd = project
        .global_config_dir
        .join("scratch/workspace-native-fixture");
    let (_, visual_cx) = cx.add_window_view(move |window, cx| {
        let shell = cx.new(|cx| {
            NativeShell::new(
                NativeShellInit {
                    runtime,
                    workspace: NativeShellWorkspaceInit::Home(Box::new(project)),
                    projectless_workspace_selection: CodingAgentWorkspaceSelection::projectless(
                        "workspace-native-fixture",
                    ),
                    global_skills: visual_global_skills(),
                    preferences,
                    preference_writer: None,
                    preference_notice: None,
                },
                window,
                cx,
            )
        });
        shell_slot_for_window.replace(Some(shell.clone()));
        gpui_component::Root::new(shell, window, cx)
    });
    let shell = shell_slot
        .borrow_mut()
        .take()
        .expect("idle visual shell entity was captured");
    (shell, visual_cx)
}

fn desktop_region_bounds(
    cx: &mut gpui::VisualTestContext,
) -> [Option<gpui::Bounds<gpui::Pixels>>; 4] {
    [
        cx.debug_bounds("desktop-sessions-panel"),
        cx.debug_bounds("desktop-conversation-panel"),
        cx.debug_bounds("desktop-composer-panel"),
        cx.debug_bounds("desktop-inspector-panel"),
    ]
}

fn assert_minimum_hit_target(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing hit-target selector {selector}"));
    assert!(
        f32::from(bounds.size.width) >= 32. && f32::from(bounds.size.height) >= 32.,
        "{selector} must retain a 32x32 desktop hit target, got {:?}",
        bounds.size
    );
}

fn choose_popup_item(cx: &mut gpui::VisualTestContext, index: usize) {
    for key in std::iter::repeat_n("down", index + 1).chain(std::iter::once("enter")) {
        let keystroke = gpui::Keystroke::parse(key)
            .unwrap_or_else(|error| panic!("popup-menu key {key} is valid: {error}"));
        let dispatched = cx.update(|window, cx| window.dispatch_keystroke(keystroke, cx));
        assert!(dispatched, "popup menu handles {key}");
        cx.run_until_parked();
    }
}

fn test_percentile_95(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn assert_composer_regions_do_not_overlap(cx: &mut gpui::VisualTestContext, notice_expected: bool) {
    let panel = cx
        .debug_bounds("desktop-composer-panel")
        .expect("Composer panel is laid out");
    let input = cx
        .debug_bounds("desktop-composer-input-region")
        .expect("Composer input region is laid out");
    let actions = cx
        .debug_bounds("desktop-composer-actions")
        .expect("Composer action region is laid out");
    assert!(input.bottom() <= actions.top());
    assert!(input.left() >= panel.left() && input.right() <= panel.right());
    assert!(actions.left() >= panel.left() && actions.right() <= panel.right());
    match cx.debug_bounds("desktop-composer-state-notice") {
        Some(notice) if notice_expected => {
            assert!(notice.bottom() <= input.top());
            assert!(notice.left() >= panel.left() && notice.right() <= panel.right());
        }
        None if !notice_expected => {}
        notice => panic!("unexpected Composer notice state: {notice:?}"),
    }
}
