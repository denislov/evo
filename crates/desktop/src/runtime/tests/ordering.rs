use super::*;

#[tokio::test]
async fn prompt_submission_forwards_product_events_and_returns_the_session_owner() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id;

    bridge
        .command_client_for_test()
        .try_submit_prompt(
            10,
            existing_prompt_target(&session_id),
            "offline desktop prompt",
            None,
        )
        .unwrap();
    let mut started_operation_id = None;
    let mut saw_product_event = false;
    let mut last_product_event_sequence = None;
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                    assert_eq!(command_id, 10);
                }
                DesktopRuntimeUpdate::PromptStarted {
                    command_id,
                    operation_id,
                    ..
                } => {
                    assert_eq!(command_id, 10);
                    started_operation_id = Some(operation_id);
                }
                DesktopRuntimeUpdate::ProductEvent { event, .. } => {
                    saw_product_event = true;
                    if let Some(previous) = last_product_event_sequence {
                        assert!(
                            event.sequence() > previous,
                            "desktop bridge reordered product event {} after {previous}",
                            event.sequence()
                        );
                    }
                    last_product_event_sequence = Some(event.sequence());
                    if let Some(started) = started_operation_id.as_deref()
                        && let Some(event_operation_id) = event.operation_id()
                    {
                        assert_eq!(event_operation_id, started);
                    }
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    snapshot,
                    ..
                } => {
                    assert_eq!(command_id, 10);
                    assert_eq!(Some(operation_id.as_str()), started_operation_id.as_deref());
                    assert_eq!(snapshot.session.session.session_id, session_id);
                    let transcript = &snapshot.transcript;
                    assert_eq!(transcript.session_id, session_id);
                    assert!(transcript.items.iter().any(|item| matches!(
                        item,
                        coding_agent::api::view::CodingAgentSessionTranscriptItem::User {
                            text
                        } if text == "offline desktop prompt"
                    )));
                    break;
                }
                DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                update => panic!("unexpected prompt update: {update:?}"),
            }
        }
    })
    .await;
    assert!(finished.is_ok(), "offline prompt did not finish promptly");
    assert!(saw_product_event);

    bridge
        .command_client_for_test()
        .try_create_session(11)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionChanged { command_id: 11, .. })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_prompts_route_events_and_completions_to_their_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, first) = start_isolated_runtime(&temp).await;
    let first_session = first.session.session.session_id;
    bridge
        .command_client_for_test()
        .try_create_session(101)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        snapshot: second, ..
    }) = bridge.next_update().await
    else {
        panic!("second session should be created");
    };
    let second_session = second.session.session.session_id;

    bridge
        .command_client_for_test()
        .try_submit_prompt(
            102,
            existing_prompt_target(&first_session),
            "first concurrent prompt",
            None,
        )
        .unwrap();
    bridge
        .command_client_for_test()
        .try_submit_prompt(
            103,
            existing_prompt_target(&second_session),
            "second concurrent prompt",
            None,
        )
        .unwrap();

    let mut accepted = std::collections::BTreeSet::new();
    let mut started = std::collections::BTreeMap::new();
    let mut finished = std::collections::BTreeMap::new();
    let mut last_sequence = std::collections::BTreeMap::<String, u64>::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while finished.len() < 2 {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                    assert!(matches!(command_id, 102 | 103));
                    accepted.insert(command_id);
                }
                DesktopRuntimeUpdate::PromptStarted {
                    command_id,
                    operation_id,
                    metadata,
                } => {
                    let session_id = metadata
                        .session
                        .expect("prompt start is session-scoped")
                        .session
                        .session_id;
                    started.insert(command_id, (session_id, operation_id));
                }
                DesktopRuntimeUpdate::ProductEvent { session_id, event } => {
                    assert!(session_id == first_session || session_id == second_session);
                    if let Some(previous) = last_sequence.insert(session_id, event.sequence()) {
                        assert!(event.sequence() > previous);
                    }
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    snapshot,
                    ..
                } => {
                    let session_id = snapshot.session.session.session_id;
                    let (started_session, started_operation) = started
                        .get(&command_id)
                        .expect("each completion must match its own start");
                    assert_eq!(&session_id, started_session);
                    assert_eq!(&operation_id, started_operation);
                    finished.insert(command_id, session_id);
                }
                DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                update => panic!("unexpected concurrent prompt update: {update:?}"),
            }
        }
    })
    .await
    .expect("both offline prompts should finish concurrently");

    assert_eq!(accepted, std::collections::BTreeSet::from([102, 103]));
    assert_eq!(finished.get(&102), Some(&first_session));
    assert_eq!(finished.get(&103), Some(&second_session));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn project_workspace_owners_isolate_context_model_profile_and_events() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let project_a = temp.path().join("project-a");
    let project_b = temp.path().join("project-b");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    write_workspace_fixture(&project_a, "project-a", "low");
    write_workspace_fixture(&project_b, "project-b", "high");
    let _env = ProcessEnvGuard::isolated(&global);
    let options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options.clone())
        .unwrap()
        .wait_blocking()
        .unwrap();

    bridge
        .command_client_for_test()
        .try_submit_prompt(
            104,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(&project_a),
                "claude-sonnet-4-5",
                "project-a",
            ),
            "project a prompt",
            None,
        )
        .unwrap();
    bridge
        .command_client_for_test()
        .try_submit_prompt(
            105,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(&project_b),
                "claude-haiku-4-5",
                "project-b",
            ),
            "project b prompt",
            None,
        )
        .unwrap();

    let mut accepted = std::collections::BTreeMap::new();
    let mut finished = std::collections::BTreeMap::new();
    let mut event_sessions = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while finished.len() < 2 {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAcceptedWithSession {
                    command_id,
                    snapshot,
                } => {
                    accepted.insert(command_id, snapshot);
                }
                DesktopRuntimeUpdate::PromptStarted { .. }
                | DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                DesktopRuntimeUpdate::ProductEvent { session_id, .. } => {
                    event_sessions.insert(session_id);
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    snapshot,
                    ..
                } => {
                    finished.insert(command_id, snapshot.session.session.session_id);
                }
                update => panic!("unexpected multi-project prompt update: {update:?}"),
            }
        }
    })
    .await
    .expect("both project-scoped prompts should finish");

    let accepted_a = accepted.get(&104).expect("project A must be accepted");
    let accepted_b = accepted.get(&105).expect("project B must be accepted");
    let canonical_a = project_a.canonicalize().unwrap();
    let canonical_b = project_b.canonicalize().unwrap();
    assert_eq!(accepted_a.project.cwd, canonical_a);
    assert_eq!(accepted_b.project.cwd, canonical_b);
    assert_eq!(accepted_a.project.selected_model_id, "claude-sonnet-4-5");
    assert_eq!(accepted_b.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        accepted_a.project.default_agent_profile_id.as_str(),
        "project-a"
    );
    assert_eq!(
        accepted_b.project.default_agent_profile_id.as_str(),
        "project-b"
    );
    assert!(
        accepted_a
            .project
            .resources
            .skill_names
            .iter()
            .any(|name| name == "project-a-skill")
    );
    assert!(
        !accepted_a
            .project
            .resources
            .skill_names
            .iter()
            .any(|name| name == "project-b-skill")
    );
    assert!(
        accepted_b
            .project
            .resources
            .context_files
            .contains(&canonical_b.join("AGENTS.md"))
    );
    assert!(
        !accepted_b
            .project
            .resources
            .context_files
            .contains(&canonical_a.join("AGENTS.md"))
    );
    let session_a = accepted_a.session.session.session_id.clone();
    let session_b = accepted_b.session.session.session_id.clone();
    assert_eq!(finished.get(&104), Some(&session_a));
    assert_eq!(finished.get(&105), Some(&session_b));
    assert_eq!(
        event_sessions,
        std::collections::BTreeSet::from([session_a.clone(), session_b.clone()])
    );

    bridge
        .command_client_for_test()
        .try_open_session(106, &session_a)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("project A owner should be focusable after prompt completion");
    };
    assert_eq!(snapshot.project.cwd, canonical_a);
    bridge
        .command_client_for_test()
        .try_select_model(107, session_owner_target(&session_a), "gpt-5", None)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SelectionChanged { metadata, .. })
            if metadata.project.cwd == canonical_a
                && metadata.project.selected_model_id == "gpt-5"
    ));

    bridge
        .command_client_for_test()
        .try_open_session(108, &session_b)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("project B owner should remain independently focusable");
    };
    assert_eq!(snapshot.project.cwd, canonical_b);
    assert_eq!(snapshot.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        snapshot.session.session.default_agent_profile_id.as_str(),
        "project-b"
    );

    bridge.shutdown().await.unwrap();

    let (mut reopened, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    for (command_id, session_id, project, prompt, skill, thinking) in [
        (
            109,
            session_a.as_str(),
            canonical_a.as_path(),
            "project a prompt",
            "project-a-skill",
            "low",
        ),
        (
            110,
            session_b.as_str(),
            canonical_b.as_path(),
            "project b prompt",
            "project-b-skill",
            "high",
        ),
        (
            111,
            session_a.as_str(),
            canonical_a.as_path(),
            "project a prompt",
            "project-a-skill",
            "low",
        ),
    ] {
        reopened
            .command_client_for_test()
            .try_open_session(command_id, session_id)
            .unwrap();
        let Some(DesktopRuntimeUpdate::SessionChanged {
            command_id: changed,
            snapshot,
        }) = reopened.next_update().await
        else {
            panic!("persisted cross-project session should reopen");
        };
        assert_eq!(changed, command_id);
        assert_eq!(snapshot.project.cwd, project);
        assert_eq!(
            snapshot.project.settings.default_thinking_level.as_deref(),
            Some(thinking)
        );
        assert!(
            snapshot
                .project
                .resources
                .skill_names
                .iter()
                .any(|name| name == skill)
        );
        assert!(snapshot.transcript.items.iter().any(|item| matches!(
            item,
            CodingAgentSessionTranscriptItem::User { text } if text == prompt
        )));
    }
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn persisted_sessions_in_one_project_receive_independent_runtime_owners() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let project = temp.path().join("shared-project");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    write_workspace_fixture(&project, "shared-project", "high");
    let _env = ProcessEnvGuard::isolated(&global);
    let project_context = CodingAgentEmbeddingContext::load(
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(
            &project,
        ))
        .unwrap()
        .with_session_dir(&sessions)
        .with_model_id("claude-sonnet-4-5"),
    )
    .unwrap();
    let mut first = project_context.create_session().await.unwrap();
    let first_id = first.view().session_id;
    first.shutdown().await.unwrap();
    let mut second = project_context.create_session().await.unwrap();
    let second_id = second.view().session_id;
    second.shutdown().await.unwrap();
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(home_options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    bridge
        .command_client_for_test()
        .try_open_session(112, &first_id)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. })
            if snapshot.project.cwd == project.canonicalize().unwrap()
    ));
    bridge
        .command_client_for_test()
        .try_select_model(113, session_owner_target(&first_id), "gpt-5", None)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SelectionChanged { metadata, .. })
            if metadata.project.selected_model_id == "gpt-5"
    ));
    bridge
        .command_client_for_test()
        .try_select_session_profile(114, session_owner_target(&first_id), "shared-project")
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SelectionChanged { metadata, .. })
            if metadata
                .session
                .as_ref()
                .is_some_and(|session| session.session.default_agent_profile_id.as_str()
                    == "shared-project")
    ));
    bridge
        .command_client_for_test()
        .try_open_session(115, &second_id)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        snapshot: second_snapshot,
        ..
    }) = bridge.next_update().await
    else {
        panic!("second session in the same project should receive an owner");
    };
    assert_eq!(
        second_snapshot.project.selected_model_id,
        "claude-sonnet-4-5"
    );
    assert_eq!(
        second_snapshot
            .session
            .session
            .default_agent_profile_id
            .as_str(),
        "default"
    );
    bridge
        .command_client_for_test()
        .try_open_session(116, &first_id)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. })
            if snapshot.project.selected_model_id == "gpt-5"
                && snapshot.session.session.default_agent_profile_id.as_str()
                    == "shared-project"
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn streaming_batch_waits_only_for_data_and_flushes_on_priority_delivery() {
    let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    let (shutdown, _shutdown_rx) = watch::channel(false);
    let (priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let mut bridge = DesktopRuntimeBridge {
        shutdown: DesktopRuntimeShutdownGuard {
            shutdown,
            runtime_thread: None,
        },
        command_client: Some(RuntimeCommandClient { commands }),
        events: DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        },
    };
    let fixture = cross_adapter_fixture_events();
    let data = fixture
        .iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .expect("fixture must contain a coalescible data event");
    let priority = fixture
        .iter()
        .find(|event| event.delivery_class() != CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .expect("fixture must contain an immediate event");

    data_tx
        .send(DesktopRuntimeUpdate::product_event(data.clone()))
        .await
        .unwrap();
    let priority_task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        priority_tx
            .send(DesktopRuntimeUpdate::product_event(priority.clone()))
            .await
            .unwrap();
        priority
    });
    let batch = bridge.next_update_batch().await.unwrap();
    let priority = priority_task.await.unwrap();

    assert_eq!(batch.len(), 2);
    assert!(matches!(
        &batch[0],
        DesktopRuntimeUpdate::ProductEvent { event, .. } if event == &data
    ));
    assert!(matches!(
        &batch[1],
        DesktopRuntimeUpdate::ProductEvent { event, .. } if event == &priority
    ));
}

#[tokio::test]
async fn priority_and_data_merge_never_compares_sequences_across_sessions() {
    let (priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let fixture = cross_adapter_fixture_events();
    let data = fixture
        .iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .unwrap();
    let priority = fixture
        .iter()
        .find(|event| event.delivery_class() != CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .unwrap();
    data_tx
        .send(DesktopRuntimeUpdate::ProductEvent {
            session_id: "session-data".into(),
            event: data,
        })
        .await
        .unwrap();
    priority_tx
        .send(DesktopRuntimeUpdate::ProductEvent {
            session_id: "session-priority".into(),
            event: priority,
        })
        .await
        .unwrap();
    let mut events = DesktopRuntimeEventStream {
        priority_updates,
        data_updates,
        pending_priority_update: None,
        pending_data_update: None,
    };

    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::ProductEvent { session_id, .. })
            if session_id == "session-priority"
    ));
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::ProductEvent { session_id, .. })
            if session_id == "session-data"
    ));
}

#[test]
fn streaming_batch_timer_does_not_require_a_tokio_reactor() {
    let (_priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let data = cross_adapter_fixture_events()
        .into_iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .expect("fixture must contain a coalescible data event");
    data_tx
        .try_send(DesktopRuntimeUpdate::product_event(data))
        .unwrap();
    let mut events = DesktopRuntimeEventStream {
        priority_updates,
        data_updates,
        pending_priority_update: None,
        pending_data_update: None,
    };

    let mut future = std::pin::pin!(events.next_update_batch());
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let batch = loop {
        match std::future::Future::poll(future.as_mut(), &mut context) {
            std::task::Poll::Ready(batch) => break batch.expect("data update should be ready"),
            std::task::Poll::Pending => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "executor-neutral coalescing timer did not complete"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };

    assert_eq!(batch.len(), 1);
}
