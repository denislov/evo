use super::*;

#[tokio::test]
async fn closing_one_active_session_does_not_interrupt_another_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, first) = start_isolated_runtime(&temp).await;
    let first_session = first.session.session.session_id;
    runtime_commands(&bridge).try_create_session(121).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        snapshot: second, ..
    }) = bridge.next_update().await
    else {
        panic!("second session should be created");
    };
    let second_session = second.session.session.session_id;
    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            122,
            existing_prompt_target(&first_session),
            "close this prompt",
            &[],
            None,
        )
        .unwrap();
    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            123,
            existing_prompt_target(&second_session),
            "keep this prompt",
            &[],
            None,
        )
        .unwrap();
    runtime_commands(&bridge)
        .try_close_session(124, &first_session)
        .unwrap();

    let mut closed = false;
    let mut second_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !closed || !second_finished {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::SessionClosed {
                    command_id: 124,
                    session_id,
                } => {
                    assert_eq!(session_id, first_session);
                    closed = true;
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id: 123,
                    snapshot,
                    ..
                } => {
                    assert_eq!(snapshot.session.session.session_id, second_session);
                    second_finished = true;
                }
                DesktopRuntimeUpdate::RuntimeFailed { error } => {
                    panic!("closing one session failed the shared runtime: {error:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the surviving prompt should finish after the other session closes");
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_sender_loss_stops_and_joins_the_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, _) = start_isolated_runtime(&temp).await;
    drop(bridge.command_client.take());

    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(update) = bridge.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                return;
            }
        }
        panic!("runtime closed without publishing Stopped");
    })
    .await;
    assert!(stopped.is_ok(), "command sender loss did not stop runtime");
    bridge.join_runtime_thread().unwrap();
}

#[tokio::test]
async fn split_runtime_owners_deliver_commands_then_shutdown_and_join() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let initial_session_id = initial.session.session.session_id;
    let (commands, mut events, shutdown) = bridge.into_parts();

    commands
        .try_reload(60, session_owner_target(&initial_session_id))
        .unwrap();
    let DesktopRuntimeUpdate::Reloaded {
        command_id,
        metadata,
    } = events.next_update().await.unwrap()
    else {
        panic!("the split event owner must deliver the command result");
    };
    assert_eq!(command_id, 60);
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        initial_session_id
    );

    shutdown.shutdown(&mut events).await.unwrap();
    assert_eq!(
        commands.try_reload(61, session_owner_target(&initial_session_id)),
        Err(DesktopCommandAdmissionError::RuntimeClosed),
        "a successful shutdown join must close the independently held command sender"
    );
}

#[tokio::test]
async fn explicit_shutdown_signal_stops_the_runtime_before_guard_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, _) = start_isolated_runtime(&temp).await;
    let (_commands, mut events, shutdown) = bridge.into_parts();
    let signal = shutdown.signal_handle();

    signal.signal();
    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(update) = events.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                return;
            }
        }
        panic!("explicit shutdown signal closed without publishing Stopped");
    })
    .await;
    assert!(
        stopped.is_ok(),
        "explicit shutdown signal did not stop runtime"
    );
    shutdown.shutdown(&mut events).await.unwrap();
}

#[tokio::test]
async fn shutdown_deadline_aborts_a_stuck_prompt_task() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let context = CodingAgentEmbeddingContext::load(options).unwrap();
    let mut session = context.create_session().await.unwrap();
    let connection = session
        .connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID))
        .unwrap();
    let requested_after = connection.state().unwrap().cursor.last_event_sequence;
    let (events, pending_recovery) = reconnect_event_source(&connection, requested_after).unwrap();
    let task = task::spawn(std::future::pending::<PromptTaskOutput>());
    let scope = context.snapshot().workspace.as_ref().unwrap().scope.clone();
    let active = ActivePrompt {
        session_id: session.view().session_id.clone(),
        command_id: 30,
        operation_id: Some("stuck-operation".into()),
        scope,
        context,
        connection,
        events,
        pending_recovery,
        last_forwarded_sequence: requested_after,
        session_name_updates: None,
        task,
    };
    let switch = dispatch_active_command(
        &active,
        DesktopRuntimeCommand::CreateSession { command_id: 31 },
    );
    assert!(matches!(
        switch,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 31,
            command: DesktopRuntimeCommandKind::CreateSession,
            ref code,
            ..
        } if code == "busy"
    ));
    for (command, expected_kind) in [
        (
            DesktopRuntimeCommand::SelectModel {
                command_id: 32,
                target: session_owner_target(active.session_id.clone()),
                model_id: "claude-haiku-4-5".into(),
                thinking_level: None,
            },
            DesktopRuntimeCommandKind::SelectModel,
        ),
        (
            DesktopRuntimeCommand::SelectSessionProfile {
                command_id: 33,
                target: session_owner_target(active.session_id.clone()),
                profile_id: "review".into(),
            },
            DesktopRuntimeCommandKind::SelectSessionProfile,
        ),
    ] {
        assert!(matches!(
            dispatch_active_command(&active, command),
            DesktopRuntimeUpdate::CommandRejected {
                command,
                ref code,
                ..
            } if command == expected_kind && code == "busy"
        ));
    }
    let stale_authorization = dispatch_active_command(
        &active,
        DesktopRuntimeCommand::DecideToolAuthorization {
            command_id: 34,
            session_id: None,
            identity: ToolAuthorizationIdentity {
                authorization_id: "already-resolved".into(),
                operation_id: "stuck-operation".into(),
                turn_id: "turn-34".into(),
                tool_call_id: "tool-call-34".into(),
                capability_generation: 1,
            },
            decision: ToolAuthorizationDecision::Deny { reason: None },
        },
    );
    assert!(matches!(
        stale_authorization,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 34,
            command: DesktopRuntimeCommandKind::DecideToolAuthorization,
            ref code,
            ..
        } if code == "input"
    ));
    let (priority_updates, mut priority_rx) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);

    shutdown_active_prompt_with_deadline(Some(active), &priority_updates, Duration::ZERO).await;
    let DesktopRuntimeUpdate::RuntimeFailed { error } = priority_rx.recv().await.unwrap() else {
        panic!("deadline expiry must publish a runtime failure");
    };
    assert_eq!(error.code, "shutdown_deadline_exceeded");
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_thread_panic_is_reported_during_join() {
    let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    drop(command_rx);
    let (shutdown, shutdown_rx) = watch::channel(false);
    drop(shutdown_rx);
    let (priority_updates_tx, priority_updates) =
        mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    drop(priority_updates_tx);
    let (data_updates_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    drop(data_updates_tx);
    let runtime_thread = thread::spawn(|| panic!("injected desktop runtime panic"));
    let bridge = DesktopRuntimeBridge {
        shutdown: DesktopRuntimeShutdownGuard {
            shutdown,
            runtime_thread: Some(runtime_thread),
        },
        command_client: Some(RuntimeCommandClient { commands }),
        events: DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        },
    };

    assert!(matches!(
        bridge.shutdown().await,
        Err(DesktopRuntimeShutdownError::RuntimePanicked)
    ));
}

#[tokio::test]
async fn abort_race_is_typed_and_window_close_is_non_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id;
    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            20,
            existing_prompt_target(&session_id),
            "abort race",
            &[],
            None,
        )
        .unwrap();

    let mut saw_control_result = false;
    let mut saw_prompt_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptStarted { .. } => {
                    runtime_commands(&bridge)
                        .try_abort_for_session(21, &session_id)
                        .unwrap();
                }
                DesktopRuntimeUpdate::ControlAccepted { command_id: 21, .. }
                | DesktopRuntimeUpdate::CommandRejected { command_id: 21, .. } => {
                    saw_control_result = true
                }
                DesktopRuntimeUpdate::PromptFinished { command_id: 20, .. } => {
                    saw_prompt_finished = true
                }
                _ => {}
            }
            if saw_control_result && saw_prompt_finished {
                break;
            }
        }
    })
    .await
    .expect("abort race must converge to a prompt terminal");
    assert!(
        saw_control_result,
        "abort command must receive a typed result"
    );
    assert!(saw_prompt_finished);

    runtime_commands(&bridge)
        .try_abort_for_session(24, &session_id)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 24,
            command: DesktopRuntimeCommandKind::Abort,
            ..
        })
    ));

    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            22,
            existing_prompt_target(&session_id),
            "close during prompt",
            &[],
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                bridge.next_update().await,
                Some(DesktopRuntimeUpdate::PromptAccepted { command_id: 22 })
            ) {
                break;
            }
        }
    })
    .await
    .expect("terminal ProductEvent acknowledgement must release the next submission slot");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || drop(bridge)),
    )
    .await
    .expect("dropping the desktop window bridge must return promptly")
    .unwrap();
}
