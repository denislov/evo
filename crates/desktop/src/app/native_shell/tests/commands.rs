use super::*;

#[gpui::test]
fn unsupported_thinking_cannot_be_selected_outside_the_menu(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    shell.update(cx, |shell, cx| {
        let selected_model_id = shell
            .app
            .workspaces
            .active_mut()
            .project
            .selected_model_id
            .clone();
        let selected = shell
            .app
            .workspaces
            .active_mut()
            .project
            .models
            .iter_mut()
            .find(|model| model.id == selected_model_id)
            .expect("the fixture selected model exists");
        selected.thinking_capability = CodingAgentThinkingCapability {
            supported: true,
            explicit_levels: vec![CodingAgentThinkingLevel::Low],
            can_disable: false,
        };

        shell.select_thinking_level(DesktopThinkingLevel::High, cx);
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Default
        );
        shell.select_thinking_level(DesktopThinkingLevel::Off, cx);
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Default
        );
        shell.select_thinking_level(DesktopThinkingLevel::Low, cx);
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Low
        );
    });
}

#[gpui::test]
fn model_switch_fallback_commits_auto_and_uses_a_header_local_hint(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    shell.update(cx, |shell, cx| {
        shell.select_thinking_level(DesktopThinkingLevel::High, cx);
        shell.submit_selection(
            DesktopRuntimeSelectionKind::Model,
            "adjacent-model".into(),
            cx,
        );
    });
    assert_eq!(
        runtime_harness.drain_selections(),
        [(
            desktop::runtime::DesktopRuntimeCommandKind::SelectModel,
            DesktopRuntimeOwnerTarget::session("desktop-visual-test"),
            "adjacent-model".into(),
            Some(CodingAgentThinkingLevel::High),
        )]
    );

    shell.update(cx, |shell, cx| {
        let mut snapshot = visual_test_snapshot();
        snapshot.project.selected_model_id = "adjacent-model".into();
        for model in &mut snapshot.project.models {
            model.selected = model.id == "adjacent-model";
        }
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::SelectionChanged {
                command_id: 1,
                selection: DesktopRuntimeSelectionKind::Model,
                thinking_level: None,
                thinking_fallback: true,
                metadata: desktop::runtime::DesktopRuntimeMetadataSnapshot {
                    project: snapshot.project,
                    session: Some(snapshot.session),
                },
            },
        );
        shell.poll_runtime_for_test(cx);

        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Default
        );
        assert_eq!(
            shell
                .app
                .preferences
                .thinking_level_for_session("desktop-visual-test"),
            DesktopThinkingLevel::Default
        );
        assert_eq!(
            shell.app.workspaces.active().thinking_hint.as_deref(),
            Some("Thinking reset to Auto for the selected model.")
        );
        assert!(
            !shell
                .app
                .workspaces
                .active()
                .preference_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("Thinking") || notice.contains("Auto"))
        );
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("desktop-composer-model-selector").is_some(),
        "the model selector remains available in the Composer"
    );
    assert!(cx.debug_bounds("desktop-composer-thinking-hint").is_some());
}

#[gpui::test]
fn composer_thinking_selector_submits_the_session_thinking_level_with_the_prompt(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .thinking_selection),
        DesktopThinkingLevel::Default
    );
    let selector = cx
        .debug_bounds("desktop-composer-thinking-selector")
        .expect("the Composer exposes an independent Thinking selector");

    cx.simulate_click(selector.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    // Auto/Off/Minimal/Low/Medium/High/XHigh.
    choose_popup_item(cx, 5);

    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .thinking_selection),
        DesktopThinkingLevel::High
    );
    shell.update(cx, |shell, cx| {
        assert_eq!(
            shell
                .app
                .preferences
                .thinking_level_for_session("desktop-visual-test"),
            DesktopThinkingLevel::High
        );
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("use the session thinking level");
        shell.submit_composer(cx);
    });

    assert_eq!(
        runtime_harness.drain_prompts(),
        [(
            DesktopPromptTarget::existing("desktop-visual-test"),
            "use the session thinking level".into(),
            Some(CodingAgentThinkingLevel::High),
        )]
    );
}

#[gpui::test]
fn composer_picker_attaches_bounded_paths_and_forwards_them_with_the_prompt(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    let add = cx
        .debug_bounds("desktop-hit-add-composer-attachments")
        .expect("composer bottom row exposes the attachment picker");
    cx.simulate_click(add.center(), gpui::Modifiers::default());
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files);
        assert!(!options.directories);
        assert!(options.multiple);
        Some(vec![
            PathBuf::from("/desktop-visual-test/screenshot.png"),
            PathBuf::from("/desktop-visual-test/notes.txt"),
        ])
    });
    cx.run_until_parked();

    shell.update(cx, |shell, cx| {
        assert_eq!(
            shell.app.workspaces.active_mut().composer_attachments.len(),
            2
        );
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("inspect the selected files");
        shell.submit_composer(cx);
    });
    assert_eq!(
        runtime_harness.drain_prompt_attachments(),
        [(
            DesktopPromptTarget::existing("desktop-visual-test"),
            "inspect the selected files".into(),
            vec![
                PathBuf::from("/desktop-visual-test/screenshot.png"),
                PathBuf::from("/desktop-visual-test/notes.txt"),
            ],
        )]
    );
}

#[gpui::test]
fn project_directory_picker_failures_are_bounded_and_do_not_replace_selection(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        let original = PathBuf::from("/kept/project");
        assert!(set_project_directory_for_test(shell, original.clone()));
        apply_picker_result_for_test(
            shell,
            DesktopPickerKind::ProjectDirectory,
            PlatformOutcome::Completed(vec![
                PathBuf::from("/unexpected/one"),
                PathBuf::from("/unexpected/two"),
            ]),
            cx,
        );
        assert_eq!(
            shell.app.workspaces.active().draft_workspace_selection,
            CodingAgentWorkspaceSelection::project(original.clone())
        );
        assert!(
            shell
                .app
                .workspaces
                .active()
                .preference_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("more than one"))
        );

        apply_picker_result_for_test(
            shell,
            DesktopPickerKind::ProjectDirectory,
            PlatformOutcome::Failed("The directory picker could not be opened.".into()),
            cx,
        );
        assert_eq!(
            shell.app.workspaces.active().draft_workspace_selection,
            CodingAgentWorkspaceSelection::project(original)
        );
        assert_eq!(
            shell.app.workspaces.active().preference_notice.as_deref(),
            Some("The directory picker could not be opened.")
        );
    });
}

#[gpui::test]
fn prompt_admission_clones_project_selection_and_blocks_late_mutation(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let selected = tempfile::tempdir().expect("selected project fixture is created");
    let replacement = tempfile::tempdir().expect("replacement project fixture is created");
    let selected_path = selected.path().to_path_buf();
    let replacement_path = replacement.path().to_path_buf();
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    shell.update(cx, |shell, cx| {
        assert!(set_project_directory_for_test(shell, selected_path.clone()));
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("freeze this project target");
        shell.submit_composer(cx);
        assert!(!set_project_directory_for_test(shell, replacement_path));
        assert!(!shell.app.workspaces.active().project_directory_editable());
    });

    assert_eq!(
        runtime_harness.drain_prompts(),
        [(
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(selected_path.clone()),
                "test-model",
                "default",
            ),
            "freeze this project target".into(),
            None,
        )]
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .draft_workspace_selection
                .clone()
        }),
        CodingAgentWorkspaceSelection::project(selected_path)
    );
}

#[gpui::test]
fn deleted_selected_project_rejects_submit_but_retains_draft_and_selection(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let selected = tempfile::tempdir().expect("selected project fixture is created");
    let selected_path = selected.path().to_path_buf();
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    shell.update(cx, |shell, _cx| {
        assert!(set_project_directory_for_test(shell, selected_path.clone()));
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("retain after project deletion");
    });
    drop(selected);
    shell.update(cx, |shell, cx| shell.submit_composer(cx));

    assert!(runtime_harness.drain_prompts().is_empty());
    shell.read_with(cx, |shell, _| {
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "retain after project deletion"
        );
        assert!(matches!(
            shell.app.workspaces.active().composer.admission(),
            ComposerAdmission::Idle
        ));
        assert!(shell.app.workspaces.active().composer.rejection().is_some());
        assert_eq!(
            shell.app.workspaces.active().draft_workspace_selection,
            CodingAgentWorkspaceSelection::project(selected_path)
        );
    });
}

#[gpui::test]
fn accepted_first_prompt_locks_scope_and_new_conversation_resets_projectless(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let selected = tempfile::tempdir().expect("selected project fixture is created");
    let selected_path = selected.path().to_path_buf();
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        assert!(set_project_directory_for_test(shell, selected_path.clone()));
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("create the selected project session");
        let intent = DesktopCommandIntent::Prompt;
        let command_id = shell
            .reserve_command(intent)
            .expect("the Home prompt fits the command ledger");
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(command_id, ComposerSubmissionKind::Prompt)
            .expect("the Home draft enters admission");
        let mut snapshot = visual_test_snapshot_for("selected-project-session");
        snapshot.project.cwd = selected_path.clone();
        snapshot.project.workspace = Some(
            CodingAgentWorkspaceSelection::project(selected_path.clone())
                .resolve(&snapshot.project.global_config_dir)
                .expect("the selected project resolves for the session fixture"),
        );
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession {
                command_id,
                snapshot,
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
        assert!(shell.app.workspaces.active().composer.draft().is_empty());
        assert_eq!(
            shell
                .app
                .workspaces
                .active()
                .composer
                .submitted()
                .map(|submitted| submitted.command_id),
            Some(command_id),
            "the admission snapshot is not a completed durable transcript"
        );
        assert!(shell.app.workspaces.active().composer.rejection().is_none());
        assert_eq!(
            shell.app.workspaces.active().project_directory(),
            Some(selected_path.as_path())
        );
        assert!(!shell.app.workspaces.active().project_directory_editable());
    });

    let new_conversation = cx
        .debug_bounds("desktop-hit-new-conversation")
        .expect("the Sidebar exposes New conversation");
    cx.simulate_click(new_conversation.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(shell.app.workspaces.active().projection.is_none());
        assert!(matches!(
            shell.app.workspaces.active().draft_workspace_selection,
            CodingAgentWorkspaceSelection::Projectless { .. }
        ));
        assert!(shell.app.workspaces.active().project_directory().is_none());
    });
}

#[gpui::test]
fn temporarily_opening_a_session_preserves_the_home_project_draft(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    let selected = PathBuf::from("/home/draft/project");
    shell.update(cx, |shell, _cx| {
        assert!(set_project_directory_for_test(shell, selected.clone()));
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("keep the scoped Home draft");
        shell
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .push(PathBuf::from("/tmp/home-owner.txt"));
        let snapshot = visual_test_snapshot_for("temporary-history-session");
        let projection = DesktopProjection::new(snapshot.clone())
            .expect("history session fixture is a valid projection");
        let history = make_session_workspace(snapshot.project, Some(projection), None);
        insert_session_workspace(shell, "temporary-history-session", history);
        assert!(activate_session(shell, "temporary-history-session"));
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("history draft");
        shell
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .push(PathBuf::from("/tmp/history-owner.txt"));
        assert!(shell.app.workspaces.activate(&WorkspaceKey::Home));
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "keep the scoped Home draft"
        );
        assert_eq!(
            shell.app.workspaces.active().draft_workspace_selection,
            CodingAgentWorkspaceSelection::project(selected)
        );
        assert_eq!(
            shell.app.workspaces.active().composer_attachments,
            [PathBuf::from("/tmp/home-owner.txt")]
        );
        assert!(activate_session(shell, "temporary-history-session"));
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "history draft"
        );
        assert_eq!(
            shell.app.workspaces.active().composer_attachments,
            [PathBuf::from("/tmp/history-owner.txt")]
        );
    });
}

#[gpui::test]
fn composer_rejects_attachment_overflow_without_changing_the_draft(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("retain this exact draft");
        apply_picker_result_for_test(
            shell,
            DesktopPickerKind::Attachments,
            PlatformOutcome::Completed(
                (0..=MAX_PROMPT_ATTACHMENTS)
                    .map(|index| PathBuf::from(format!("/tmp/attachment-{index}.png")))
                    .collect(),
            ),
            cx,
        );
        assert!(
            shell
                .app
                .workspaces
                .active()
                .composer_attachments
                .is_empty()
        );
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "retain this exact draft"
        );
        assert!(
            shell
                .app
                .workspaces
                .active()
                .preference_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("more than 16 attachments"))
        );
    });
}

#[gpui::test]
fn switching_workspaces_restores_each_persisted_thinking_level(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let snapshot_a = visual_test_snapshot_for("thinking-session-a");
    let projection_a = DesktopProjection::new(snapshot_a)
        .expect("thinking session A fixture is a valid projection");
    let mut preferences = DesktopPreferences::default();
    assert!(
        preferences
            .set_thinking_level_for_session("thinking-session-a", DesktopThinkingLevel::High)
    );
    assert!(
        preferences.set_thinking_level_for_session("thinking-session-b", DesktopThinkingLevel::Low)
    );
    let (shell, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_a,
        preferences,
    );
    cx.run_until_parked();

    shell.update(cx, |shell, cx| {
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::High
        );
        let snapshot_b = visual_test_snapshot_for("thinking-session-b");
        let projection_b = DesktopProjection::new(snapshot_b.clone())
            .expect("thinking session B fixture is a valid projection");
        let thinking_b = shell
            .app
            .preferences
            .thinking_level_for_session("thinking-session-b");
        insert_session_workspace(
            shell,
            "thinking-session-b",
            session_workspace_with_thinking(
                snapshot_b.project,
                Some(projection_b),
                None,
                thinking_b,
            ),
        );

        assert!(activate_session(shell, "thinking-session-b"));
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Low
        );
        shell.select_thinking_level(DesktopThinkingLevel::XHigh, cx);
        assert_eq!(
            shell
                .app
                .preferences
                .thinking_level_for_session("thinking-session-b"),
            DesktopThinkingLevel::XHigh
        );

        assert!(activate_session(shell, "thinking-session-a"));
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::High
        );
        assert!(activate_session(shell, "thinking-session-b"));
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::XHigh
        );
    });
}

#[gpui::test]
fn hydration_restores_existing_thinking_but_new_sessions_inherit_home(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);

    shell.update(cx, |shell, _| {
        assert!(shell.app.preferences.set_thinking_level_for_session(
            "existing-thinking-session",
            DesktopThinkingLevel::Low,
        ));
        shell.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::XHigh;
        let existing = visual_test_snapshot_for("existing-thinking-session");

        assert!(shell.app.install_hydrated_workspace(&existing, false, true));
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Low
        );
        assert_eq!(
            shell
                .app
                .preferences
                .thinking_level_for_session("existing-thinking-session"),
            DesktopThinkingLevel::Low
        );

        assert!(shell.app.workspaces.activate(&WorkspaceKey::Home));
        shell.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::Medium;
        let created = visual_test_snapshot_for("created-thinking-session");

        assert!(shell.app.install_hydrated_workspace(&created, true, true));
        assert_eq!(
            shell.app.workspaces.active().thinking_selection,
            DesktopThinkingLevel::Medium
        );
        assert_eq!(
            shell
                .app
                .preferences
                .thinking_level_for_session("created-thinking-session"),
            DesktopThinkingLevel::Medium
        );
    });
}
