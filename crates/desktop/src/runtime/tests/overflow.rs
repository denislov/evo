use super::*;

#[tokio::test]
async fn ten_mib_transcript_stays_single_hydration_across_metadata_commands() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, mut initial) = start_isolated_runtime(&temp).await;
    let payload = "x".repeat(1_280);
    initial.transcript.items = (0..MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("{index}:{payload}"),
        })
        .collect();
    let fixture_bytes = initial
        .transcript
        .items
        .iter()
        .map(|item| match item {
            CodingAgentSessionTranscriptItem::User { text } => text.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(fixture_bytes >= 10 * 1024 * 1024);
    let metadata = DesktopRuntimeMetadataSnapshot {
        project: initial.project.clone(),
        session: Some(initial.session.clone()),
    };
    let recovery = DesktopRuntimeRecoverySnapshot {
        project: initial.project.clone(),
        session: initial.session.clone(),
        pending_recoveries: Vec::new(),
    };
    let mut projection = DesktopProjection::new(initial).unwrap();
    let initial_counters = projection.counters();
    assert_eq!(initial_counters.full_transcript_hydrations, 1);
    assert_eq!(
        initial_counters.transcript_items_hydrated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(
        initial_counters.conversation_blocks_allocated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert!(projection.conversation().retained_bytes() <= MAX_TRANSCRIPT_BYTES);

    for command_id in 100..164 {
        let update = match command_id % 4 {
            0 => DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata: metadata.clone(),
            },
            1 => DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::Model,
                thinking_level: None,
                thinking_fallback: false,
                metadata: metadata.clone(),
            },
            2 => DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::SessionProfile,
                thinking_level: None,
                thinking_fallback: false,
                metadata: metadata.clone(),
            },
            _ => DesktopRuntimeUpdate::PromptStarted {
                command_id,
                operation_id: format!("metadata-operation-{command_id}"),
                metadata: metadata.clone(),
            },
        };
        assert!(
            projection
                .apply(
                    crate::application::reducer::projection_event(update)
                        .expect("metadata fixture must map to a projection event"),
                )
                .is_replaced()
        );
    }
    for _ in 164..180 {
        assert!(
            projection
                .apply(ProjectionEvent::Recovery(recovery.clone()))
                .is_replaced()
        );
    }

    let counters = projection.counters();
    assert_eq!(counters.full_transcript_hydrations, 1);
    assert_eq!(
        counters.transcript_items_hydrated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(
        counters.conversation_blocks_allocated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(counters.metadata_replacements, 64);
    assert_eq!(counters.recovery_replacements, 16);
    assert_eq!(
        projection.conversation().blocks().len(),
        MAX_TRANSCRIPT_BLOCKS
    );
    assert!(
        projection
            .conversation()
            .blocks()
            .front()
            .unwrap()
            .text
            .starts_with("0:")
    );
    assert!(
        projection
            .conversation()
            .blocks()
            .back()
            .unwrap()
            .text
            .starts_with("9999:")
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn data_queue_overflow_emits_a_priority_resync_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let (priority_updates, mut priority_rx) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_updates, _data_rx) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    for command_id in 0..DESKTOP_UPDATE_QUEUE_CAPACITY as u64 {
        data_updates
            .try_send(DesktopRuntimeUpdate::PromptAccepted { command_id })
            .unwrap();
    }

    assert!(
        publish_data_update(
            DesktopRuntimeUpdate::PromptAccepted {
                command_id: u64::MAX,
            },
            || Ok::<_, DesktopBridgeError>(initial.session.clone()),
            &priority_updates,
            &data_updates,
        )
        .await
    );
    let DesktopRuntimeUpdate::ResyncRequired { reason, snapshot } =
        priority_rx.recv().await.unwrap()
    else {
        panic!("data overflow must publish a priority resync request");
    };
    assert_eq!(reason.code, "desktop_data_queue_full");
    assert_eq!(
        snapshot.session.session_id,
        initial.session.session.session_id
    );
    bridge.shutdown().await.unwrap();
}

#[test]
fn command_inputs_and_queue_capacities_are_bounded() {
    assert!((1..=128).contains(&DESKTOP_COMMAND_QUEUE_CAPACITY));
    assert!((1..=256).contains(&DESKTOP_UPDATE_QUEUE_CAPACITY));
    assert!((1..=128).contains(&DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY));
    assert!(validate_session_id("").is_err());
    assert!(validate_session_id(&"x".repeat(MAX_SESSION_ID_BYTES + 1)).is_err());
    assert!(validate_session_id("session-ok").is_ok());
    assert!(validate_prompt("").is_err());
    assert!(validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
    assert!(validate_prompt("prompt").is_ok());
    let attachments = vec![std::path::PathBuf::from("fixture.png"); MAX_PROMPT_ATTACHMENTS];
    assert!(validate_prompt_with_attachments("", &attachments).is_ok());
    let over_limit = vec![std::path::PathBuf::from("fixture.png"); MAX_PROMPT_ATTACHMENTS + 1];
    assert!(validate_prompt_with_attachments("draft remains", &over_limit).is_err());
    assert!(validate_control_text("").is_err());
    assert!(validate_control_text(&"x".repeat(MAX_CONTROL_TEXT_BYTES + 1)).is_err());
    assert!(validate_control_text("steer").is_ok());
    let mut identity = ToolAuthorizationIdentity {
        authorization_id: "authorization-ok".into(),
        operation_id: "operation-ok".into(),
        turn_id: "turn-ok".into(),
        tool_call_id: "tool-call-ok".into(),
        capability_generation: 1,
    };
    assert!(validate_authorization_identity(&identity).is_ok());
    identity.authorization_id.clear();
    assert!(validate_authorization_identity(&identity).is_err());
    identity.authorization_id = "authorization-ok".into();
    identity.tool_call_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
    assert!(validate_authorization_identity(&identity).is_err());
    let mut recovery = DesktopRecoveryIdentity {
        operation_id: "operation-ok".into(),
        recovery_id: "recovery-ok".into(),
        record_version: 1,
        descriptor_revision: 1,
        capability_generation: Some(1),
        attempt_count: 0,
    };
    assert!(validate_recovery_identity(&recovery).is_ok());
    recovery.recovery_id.clear();
    assert!(validate_recovery_identity(&recovery).is_err());
    recovery.recovery_id = "x".repeat(MAX_RECOVERY_ID_BYTES + 1);
    assert!(validate_recovery_identity(&recovery).is_err());
    assert!(validate_selection_id("model", "").is_err());
    assert!(validate_selection_id("profile", &"x".repeat(MAX_SELECTION_ID_BYTES + 1)).is_err());
    assert!(validate_selection_id("model", "claude-haiku-4-5").is_ok());
}

#[test]
fn prompt_target_admission_is_typed_bounded_and_debug_redacted() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("private-project-name");
    std::fs::create_dir_all(&project).unwrap();
    let valid = DesktopPromptTarget::new(
        CodingAgentWorkspaceSelection::project(&project),
        "private-model-id",
        "private-profile-id",
    );
    assert!(validate_prompt_target(&valid).is_ok());
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::existing("")),
        Err(DesktopCommandAdmissionError::InvalidSessionId { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(temp.path().join("missing-project")),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    let file = temp.path().join("not-a-project-directory");
    std::fs::write(&file, "file").unwrap();
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(file),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project("bad\0project"),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(
                std::path::PathBuf::from("x").join("y".repeat(MAX_WORKSPACE_PATH_BYTES))
            ),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(&project),
            "",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidSelectionId { .. })
    ));

    let command = DesktopRuntimeCommand::SubmitPrompt {
        command_id: 902,
        target: valid,
        prompt: "private prompt body".into(),
        attachments: vec![std::path::PathBuf::from("private-attachment-name")],
        thinking_level: None,
    };
    let debug = format!("{command:?}");
    assert!(debug.contains("SubmitPrompt"));
    assert!(debug.contains("new"));
    for secret in [
        "private-project-name",
        "private-model-id",
        "private-profile-id",
        "private prompt body",
        "private-attachment-name",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn attachment_commands_preserve_bounded_paths_and_session_target() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("project")).unwrap();
    let (bridge, mut harness) = DesktopRuntimeBridge::instrumented_for_test();
    let attachments = [
        std::path::PathBuf::from("screenshots/one.png"),
        std::path::PathBuf::from("notes/two.txt"),
    ];
    bridge
        .command_client_for_test()
        .try_submit_prompt_with_attachments(
            900,
            existing_prompt_target("session-attachment-test"),
            "inspect these",
            &attachments,
            None,
        )
        .unwrap();
    bridge
        .command_client_for_test()
        .try_submit_prompt_with_attachments(
            901,
            new_project_prompt_target(&temp),
            "inspect once more",
            &attachments[..1],
            None,
        )
        .unwrap();
    assert_eq!(
        harness.drain_prompt_attachments(),
        [
            (
                existing_prompt_target("session-attachment-test"),
                "inspect these".into(),
                attachments.to_vec(),
            ),
            (
                new_project_prompt_target(&temp),
                "inspect once more".into(),
                attachments[..1].to_vec(),
            ),
        ]
    );
}
