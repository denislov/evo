use super::*;

#[tokio::test]
async fn desktop_projection_rejects_gaps_and_association_mismatches_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id.clone();
    let mut wrong_transcript = initial.clone();
    wrong_transcript.transcript.session_id = "wrong-session".into();
    assert_eq!(
        DesktopProjection::new(wrong_transcript).unwrap_err().code,
        "transcript_session_mismatch"
    );
    let mut projection = DesktopProjection::new(initial).unwrap();
    bridge
        .command_client_for_test()
        .try_submit_prompt(
            40,
            existing_prompt_target(session_id),
            "projection cursor fixture",
            None,
        )
        .unwrap();

    let mut exercised_strict_reducer = false;
    let mut requested_active_resync = false;
    let mut saw_active_resync = false;
    let mut saw_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = bridge.next_update().await.unwrap();
            if matches!(update, DesktopRuntimeUpdate::PromptStarted { .. })
                && !requested_active_resync
            {
                bridge.command_client_for_test().try_resync(41).unwrap();
                requested_active_resync = true;
            }
            if let DesktopRuntimeUpdate::Resynced { command_id: 41, .. } = &update {
                saw_active_resync = true;
            }
            if let DesktopRuntimeUpdate::ProductEvent { event, .. } = &update
                && !exercised_strict_reducer
            {
                let mut baseline = projection.clone();
                let expected = baseline.cursor().last_event_sequence + 1;
                let submitted_operation = baseline
                    .snapshot()
                    .submitted_operation
                    .as_ref()
                    .map(|operation| operation.operation_id.clone());

                let valid = rewritten_event(
                    event,
                    expected,
                    baseline.cursor().stream_id.as_str(),
                    Some(baseline.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert!(
                    baseline
                        .apply(ProjectionEvent::Product(valid.clone()))
                        .is_applied()
                );
                assert_eq!(
                    baseline.apply(ProjectionEvent::Product(valid)),
                    DesktopProjectionApply::IgnoredDuplicate
                );

                let mut gap_projection = projection.clone();
                let original_cursor = gap_projection.cursor().clone();
                let gap = rewritten_event(
                    event,
                    expected + 1,
                    gap_projection.cursor().stream_id.as_str(),
                    Some(gap_projection.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    gap_projection.apply(ProjectionEvent::Product(gap)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(gap_projection.cursor(), &original_cursor);
                assert_eq!(
                    gap_projection.lifecycle(),
                    DesktopProjectionLifecycle::NeedsResync
                );
                assert!(
                    gap_projection
                        .apply(ProjectionEvent::ProductSnapshot {
                            reason: DesktopRuntimeError {
                                code: "test_resync".into(),
                                message: "replace after an injected cursor gap".into(),
                            },
                            snapshot: projection.snapshot().clone(),
                        })
                        .is_replaced()
                );
                assert_eq!(
                    gap_projection.lifecycle(),
                    DesktopProjectionLifecycle::Running
                );
                assert!(gap_projection.recent_events().is_empty());

                let mut wrong_session = projection.clone();
                let mismatched = rewritten_event(
                    event,
                    expected,
                    wrong_session.cursor().stream_id.as_str(),
                    Some("another-session"),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    wrong_session.apply(ProjectionEvent::Product(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_session.issues().back().unwrap().code,
                    "product_event_session_mismatch"
                );

                let mut wrong_stream = projection.clone();
                let mismatched = rewritten_event(
                    event,
                    expected,
                    "another-stream",
                    Some(wrong_stream.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    wrong_stream.apply(ProjectionEvent::Product(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_stream.issues().back().unwrap().code,
                    "product_event_stream_mismatch"
                );

                let mut wrong_generation = projection.clone();
                let mut value = serde_json::to_value(rewritten_event(
                    event,
                    expected,
                    wrong_generation.cursor().stream_id.as_str(),
                    Some(wrong_generation.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                ))
                .unwrap();
                value["capability_generation"] = serde_json::json!(
                    wrong_generation
                        .cursor()
                        .capability_generation
                        .saturating_add(2)
                );
                let mismatched = serde_json::from_value(value).unwrap();
                assert_eq!(
                    wrong_generation.apply(ProjectionEvent::Product(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_generation.issues().back().unwrap().code,
                    "product_event_capability_generation_mismatch"
                );

                if submitted_operation.is_some() {
                    let mut wrong_operation = projection.clone();
                    let mismatched = rewritten_event(
                        event,
                        expected,
                        wrong_operation.cursor().stream_id.as_str(),
                        Some(wrong_operation.snapshot().session.session_id.as_str()),
                        Some("unrelated-operation"),
                    );
                    assert_eq!(
                        wrong_operation.apply(ProjectionEvent::Product(mismatched)),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(
                        wrong_operation.issues().back().unwrap().code,
                        "product_event_operation_mismatch"
                    );
                }
                assert_bounded_streaming_overlays(
                    &projection,
                    event,
                    submitted_operation.as_deref(),
                );
                exercised_strict_reducer = true;
            }

            saw_finished |= matches!(update, DesktopRuntimeUpdate::PromptFinished { .. });
            if let Some(event) = crate::application::reducer::projection_event(update) {
                let outcome = projection.apply(event);
                assert_ne!(
                    outcome,
                    DesktopProjectionApply::NeedsResync,
                    "real runtime updates must satisfy the desktop projection contract: {:?}",
                    projection.issues().back()
                );
            }
            if saw_finished && saw_active_resync {
                break;
            }
        }
    })
    .await
    .expect("projection fixture prompt must finish");
    assert!(exercised_strict_reducer);
    assert!(saw_active_resync);
    assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Running);
    assert!(
        projection
            .conversation()
            .blocks()
            .iter()
            .any(|block| block.text == "projection cursor fixture")
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_cross_adapter_fixture_matches_desktop_product_state_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let transcript = initial.transcript.clone();
    let mut shared = CodingAgentClientProjection::from_bootstrap(CodingAgentClientBootstrap {
        snapshot: initial.session.clone(),
        transcript,
        pending_recoveries: initial.pending_recoveries.clone(),
    })
    .unwrap();
    let mut desktop = DesktopProjection::new(initial).unwrap();
    let base_sequence = desktop.cursor().last_event_sequence;
    let stream_id = desktop.cursor().stream_id.clone();
    let session_id = desktop.snapshot().session.session_id.clone();

    for fixture in cross_adapter_fixture_events() {
        let event = rewritten_event(
            &fixture,
            base_sequence + fixture.sequence(),
            &stream_id,
            Some(&session_id),
            fixture.operation_id(),
        );
        assert!(matches!(
            shared.apply(&event),
            CodingAgentClientProjectionApply::Applied(_)
        ));
        let terminal = event.terminal_operation().is_some();
        let outcome = desktop.apply(ProjectionEvent::Product(event));
        assert!(outcome.is_applied());
        assert_eq!(outcome.delta().unwrap().terminal, terminal);
    }

    assert_eq!(desktop.product_for_tests(), &shared);
    assert_eq!(
        desktop
            .messages()
            .front()
            .map(|message| message.text.as_str()),
        Some("hello world")
    );
    assert_eq!(
        desktop.tools().front().map(|tool| tool.detail.as_str()),
        Some("read complete")
    );
    assert_eq!(
        desktop.snapshot().context.delegations[0].status,
        "completed"
    );
    assert_eq!(
        desktop.snapshot().session.default_agent_profile_id.as_str(),
        "reviewer"
    );
    bridge.shutdown().await.unwrap();
}

fn rewritten_event(
    event: &CodingAgentProductEvent,
    sequence: u64,
    stream_id: &str,
    session_id: Option<&str>,
    operation_id: Option<&str>,
) -> CodingAgentProductEvent {
    let mut value = serde_json::to_value(event).unwrap();
    value["sequence"] = serde_json::json!(sequence);
    value["stream_id"] = serde_json::json!(stream_id);
    value["session_id"] = session_id.map_or(serde_json::Value::Null, |session_id| {
        serde_json::json!(session_id)
    });
    value["operation_id"] = operation_id.map_or(serde_json::Value::Null, |operation_id| {
        serde_json::json!(operation_id)
    });
    value["parent_operation_id"] = serde_json::Value::Null;
    value["root_operation_id"] = serde_json::Value::Null;
    serde_json::from_value(value).unwrap()
}

fn rewritten_event_kind(
    event: &CodingAgentProductEvent,
    sequence: u64,
    stream_id: &str,
    session_id: &str,
    operation_id: &str,
    kind: serde_json::Value,
) -> CodingAgentProductEvent {
    let rewritten = rewritten_event(
        event,
        sequence,
        stream_id,
        Some(session_id),
        Some(operation_id),
    );
    let mut value = serde_json::to_value(rewritten).unwrap();
    value["event"] = kind;
    value["terminal_status"] = serde_json::Value::Null;
    value["terminal_operation"] = serde_json::Value::Null;
    serde_json::from_value(value).unwrap()
}

fn assert_bounded_streaming_overlays(
    projection: &DesktopProjection,
    base_event: &CodingAgentProductEvent,
    submitted_operation: Option<&str>,
) {
    let Some(operation_id) = submitted_operation else {
        return;
    };
    let mut overlays = projection.clone();
    let stream_id = overlays.cursor().stream_id.clone();
    let session_id = overlays.snapshot().session.session_id.clone();
    let initial_usage_input = overlays.snapshot().context.usage.input;
    let initial_usage_output = overlays.snapshot().context.usage.output;
    let initial_view_rebuilds = overlays.counters().product_view_rebuilds;
    let mut sequence = overlays.cursor().last_event_sequence;

    sequence += 1;
    let started = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "started",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay"
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(started));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.cursor);
    assert!(delta.conversation);
    assert!(!delta.tools);
    assert!(!delta.context.contains(ContextDirtyFlags::USAGE));

    sequence += 1;
    let delta = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "delta",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay",
                "text": "streaming text"
            }
        }),
    );
    assert!(overlays.apply(ProjectionEvent::Product(delta)).is_applied());

    sequence += 1;
    let completed = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "completed",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay",
                "final_text": "final text",
                "images": [],
                "usage": {
                    "input": 1,
                    "output": 2,
                    "cache_read": 0,
                    "cache_write": 0,
                    "total_tokens": 3,
                    "cost_known": false,
                    "input_cost": 0.0,
                    "output_cost": 0.0,
                    "cache_read_cost": 0.0,
                    "cache_write_cost": 0.0
                }
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(completed));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.conversation);
    assert!(delta.context.contains(ContextDirtyFlags::USAGE));
    let message = overlays.messages().back().unwrap();
    assert_eq!(message.text, "final text");
    assert_eq!(message.status, DesktopMessageStatus::Completed);
    assert_eq!(
        overlays.snapshot().context.usage.input,
        initial_usage_input + 1
    );
    assert_eq!(
        overlays.snapshot().context.usage.output,
        initial_usage_output + 2
    );

    for index in 0..=MAX_DESKTOP_MESSAGE_OVERLAYS {
        sequence += 1;
        let completed = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "completed",
                    "operation_id": operation_id,
                    "turn_id": format!("turn-{index}"),
                    "message_id": format!("message-{index}"),
                    "final_text": "bounded",
                    "images": [],
                    "usage": {
                        "input": 0,
                        "output": 0,
                        "cache_read": 0,
                        "cache_write": 0,
                        "total_tokens": 0,
                        "cost_known": false,
                        "input_cost": 0.0,
                        "output_cost": 0.0,
                        "cache_read_cost": 0.0,
                        "cache_write_cost": 0.0
                    }
                }
            }),
        );
        assert!(
            overlays
                .apply(ProjectionEvent::Product(completed))
                .is_applied()
        );
    }
    assert_eq!(overlays.messages().len(), MAX_DESKTOP_MESSAGE_OVERLAYS);

    sequence += 1;
    let tool_started = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "tool",
            "payload": {
                "kind": "started",
                "operation_id": operation_id,
                "turn_id": "turn-tool",
                "tool_call_id": "tool-overlay",
                "name": "edit",
                "arguments_json": "{\"path\":\"README.md\"}"
            }
        }),
    );
    assert!(
        overlays
            .apply(ProjectionEvent::Product(tool_started))
            .is_applied()
    );
    sequence += 1;
    let tool_completed = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "tool",
            "payload": {
                "kind": "completed",
                "operation_id": operation_id,
                "turn_id": "turn-tool",
                "tool_call_id": "tool-overlay",
                "name": "edit",
                "summary": "edited README.md"
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(tool_completed));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.tools);
    assert!(delta.context.contains(ContextDirtyFlags::CHANGES));
    assert!(!delta.conversation);
    assert_eq!(
        overlays.tools().back().unwrap().status,
        DesktopToolStatus::Completed
    );
    assert_eq!(
        overlays.snapshot().context.changes.first().unwrap().path,
        "README.md"
    );

    sequence += 1;
    let delegation = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "delegation",
            "payload": {
                "kind": "started",
                "context": {
                    "operation_id": operation_id,
                    "turn_id": "turn-delegation",
                    "tool_call_id": "delegation-overlay",
                    "requesting_profile_id": "default",
                    "target_kind": "agent",
                    "target_id": "reviewer",
                    "task": "review projection"
                },
                "child_operation_id": "child-overlay"
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(delegation));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.context.contains(ContextDirtyFlags::DELEGATIONS));
    assert!(!delta.conversation);
    assert!(!delta.tools);
    assert_eq!(
        overlays
            .snapshot()
            .context
            .delegations
            .first()
            .unwrap()
            .status,
        "running"
    );

    sequence += 1;
    let recovery = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "workflow",
            "payload": {
                "kind": "operation_recovery_pending",
                "operation_id": operation_id,
                "recovery_id": "recovery-overlay",
                "reason": "injected recovery",
                "record_version": 1,
                "descriptor_revision": 1,
                "capability_generation": null,
                "attempt_count": 0,
                "last_attempt_at": null,
                "next_attempt_at": null
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(recovery));
    assert!(outcome.is_applied());
    assert!(outcome.delta().unwrap().recoveries);
    assert_eq!(
        overlays.recoveries().front().unwrap().status,
        crate::projection::DesktopRecoveryStatus::Pending
    );

    sequence += 1;
    let diagnostic = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "diagnostic",
            "payload": {
                "kind": "diagnostic",
                "diagnostic": {
                    "severity": "warning",
                    "code": "projection_diagnostic",
                    "summary": "projection diagnostic",
                    "origin": "runtime",
                    "operation_id": operation_id
                }
            }
        }),
    );
    let outcome = overlays.apply(ProjectionEvent::Product(diagnostic));
    assert!(outcome.is_applied());
    assert!(outcome.delta().unwrap().diagnostics);
    assert_eq!(
        overlays.diagnostics().back().unwrap().message,
        "projection diagnostic"
    );
    let incremental_counters = overlays.counters();
    assert_eq!(
        incremental_counters.product_view_rebuilds, initial_view_rebuilds,
        "product events must not rebuild every compatibility view"
    );
    assert!(incremental_counters.incremental_message_updates > 1);
    assert_eq!(incremental_counters.incremental_tool_updates, 2);
    assert_eq!(incremental_counters.incremental_recovery_updates, 1);
    assert_eq!(incremental_counters.incremental_diagnostic_updates, 1);

    let mut fresh = overlays.snapshot().clone();
    fresh.cursor = overlays.cursor().clone();
    assert!(
        overlays
            .apply(ProjectionEvent::ProductSnapshot {
                reason: DesktopRuntimeError {
                    code: "overlay_resync".into(),
                    message: "discard incomplete live overlays".into(),
                },
                snapshot: fresh,
            })
            .is_replaced()
    );
    assert!(overlays.messages().is_empty());
    assert!(overlays.tools().is_empty());
    assert_eq!(
        overlays.counters().product_view_rebuilds,
        initial_view_rebuilds + 1
    );
    assert_eq!(
        overlays
            .recoveries()
            .front()
            .map(|recovery| recovery.recovery_id.as_str()),
        Some("recovery-overlay")
    );
    assert!(!overlays.recoveries().front().unwrap().authoritative);
    assert!(overlays.diagnostics().is_empty());
}

#[tokio::test]
async fn typed_recovery_reasons_replace_the_projection_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let mut projection = DesktopProjection::new(initial.clone()).unwrap();
    let cursor = initial.session.cursor.clone();

    let live_lag = recovery_update(CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence.saturating_sub(1),
        oldest_available_sequence: cursor.last_event_sequence,
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::LiveReceiverLag,
        snapshot: Box::new(initial.session.clone()),
    });
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &live_lag else {
        panic!("live lag must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_live_receiver_lag");
    assert!(
        projection
            .apply(
                crate::application::reducer::projection_event(live_lag)
                    .expect("live lag must map to a product snapshot"),
            )
            .is_replaced()
    );
    assert_eq!(
        projection
            .last_resync_reason()
            .expect("live lag reason should be retained")
            .code,
        "product_event_live_receiver_lag"
    );

    let retained_gap = recovery_update(CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor,
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session),
    });
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &retained_gap else {
        panic!("retained gap must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_retained_history_gap");
    assert!(
        projection
            .apply(
                crate::application::reducer::projection_event(retained_gap)
                    .expect("retained gap must map to a product snapshot"),
            )
            .is_replaced()
    );
    assert!(projection.recent_events().is_empty());
    assert_eq!(
        projection.apply(ProjectionEvent::Stopped),
        DesktopProjectionApply::NoDelta
    );
    assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Stopped);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_actions_are_identity_bound_and_stale_facts_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let pending = CodingAgentRecoveryPending {
        operation_id: "operation-recovery".into(),
        recovery_id: "recovery-id".into(),
        operation_kind: Some("prompt".into()),
        record_version: 3,
        descriptor_revision: 2,
        capability_generation: Some(initial.session.cursor.capability_generation),
        attempt_count: 1,
        last_attempt_at: Some("2026-07-24T00:00:00Z".into()),
        next_attempt_at: None,
    };
    let identity = DesktopRecoveryIdentity::from(&pending);
    let mut projected = initial;
    projected.pending_recoveries = vec![pending];
    let projection = DesktopProjection::new(projected).unwrap();
    let recovery = projection.recoveries().front().unwrap();
    assert!(recovery.authoritative);
    assert_eq!(recovery.identity.as_ref(), Some(&identity));
    assert_eq!(recovery.attempt_count, 1);

    bridge
        .command_client_for_test()
        .try_retry_recovery(32, &identity)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 32,
            command: DesktopRuntimeCommandKind::RetryRecovery,
            ..
        })
    ));
    bridge
        .command_client_for_test()
        .try_resolve_recovery(33, &identity, CodingAgentRecoveryResolution::Aborted)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 33,
            command: DesktopRuntimeCommandKind::ResolveRecovery,
            ..
        })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_projection_preserves_identity_and_bounds_display_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let request = ToolAuthorizationRequest {
        authorization_id: "authorization-exact".into(),
        operation_id: "operation-exact".into(),
        turn_id: "turn-exact".into(),
        tool_call_id: "tool-call-exact".into(),
        tool_name: "bash".into(),
        risk: ToolAuthorizationRisk::ShellExecution,
        scope: ToolAuthorizationScope::Shell {
            cwd: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
            command_fingerprint: "fingerprint".into(),
        },
        preview: ToolAuthorizationPreview {
            summary: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
            path: None,
            command: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
            cwd: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
            content_preview: None,
        },
        capability_generation: initial.session.cursor.capability_generation,
        requested_at: "2026-07-24T00:00:00Z".into(),
    };

    let mut bounded = initial.clone();
    bounded.session.pending_authorizations.push(request.clone());
    let projection = DesktopProjection::new(bounded).unwrap();
    let retained = projection
        .snapshot()
        .pending_authorizations
        .first()
        .unwrap();
    assert_eq!(retained.authorization_id, "authorization-exact");
    assert_eq!(retained.operation_id, "operation-exact");
    assert!(retained.preview.summary.len() <= MAX_AUTHORIZATION_TEXT_BYTES);
    assert!(retained.preview.command.as_ref().unwrap().len() <= MAX_AUTHORIZATION_TEXT_BYTES);

    let mut invalid = initial.clone();
    let mut invalid_request = request.clone();
    invalid_request.authorization_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
    invalid.session.pending_authorizations.push(invalid_request);
    assert_eq!(
        DesktopProjection::new(invalid).unwrap_err().code,
        "authorization_identity_invalid"
    );

    let mut stale = initial;
    let mut stale_request = request.clone();
    stale_request.capability_generation =
        stale_request.capability_generation.checked_add(1).unwrap();
    stale.session.pending_authorizations.push(stale_request);
    assert_eq!(
        DesktopProjection::new(stale).unwrap_err().code,
        "authorization_capability_generation_mismatch"
    );

    let identity = request.identity();
    assert_eq!(request.identity(), identity);
    let mut stale_identity = identity.clone();
    stale_identity.capability_generation =
        stale_identity.capability_generation.checked_add(1).unwrap();
    assert_ne!(request.identity(), stale_identity);
    stale_identity = identity;
    stale_identity.operation_id = "another-operation".into();
    assert_ne!(request.identity(), stale_identity);
    bridge.shutdown().await.unwrap();
}

#[test]
fn runtime_error_preserves_only_the_product_safe_error_projection() {
    let product_error = CodingAgentPublicError {
        category: CodingAgentErrorCategory::Provider,
        code: "provider".into(),
        retryable: true,
        summary: "The model provider request failed.".into(),
        context: CodingAgentErrorContext::None,
    };
    let error = runtime_error(&product_error);
    let rendered = format!("{}: {}", error.code, error.message);

    assert_eq!(error.code, "provider");
    assert_eq!(error.message, "The model provider request failed.");
    assert_eq!(rendered, "provider: The model provider request failed.");
}
