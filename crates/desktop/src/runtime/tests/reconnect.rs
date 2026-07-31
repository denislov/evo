use super::*;

#[tokio::test]
async fn reconnect_state_machine_handles_gap_lag_and_exhaustion_deterministically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let cursor = initial.session.cursor.clone();

    let retained = CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let mut attempts = VecDeque::from([
        DesktopReconnectAttempt::FreshSnapshotRequired(retained),
        DesktopReconnectAttempt::Replayed {
            events: Vec::new(),
            receiver: (),
        },
    ]);
    let mut requested = Vec::new();
    let (events, (), recovery) = establish_reconnect(0, |sequence| {
        requested.push(sequence);
        Ok(attempts
            .pop_front()
            .expect("two reconnect attempts should be consumed"))
    })
    .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        requested,
        vec![0, cursor.last_event_sequence],
        "fresh snapshot cursor must anchor the second reconnect"
    );
    assert_eq!(
        recovery.unwrap().reason,
        CodingAgentRecoveryReason::RetainedHistoryGap
    );

    let first = CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: 1,
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let second = CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let mut attempts = VecDeque::from([
        DesktopReconnectAttempt::<()>::FreshSnapshotRequired(first),
        DesktopReconnectAttempt::<()>::FreshSnapshotRequired(second),
    ]);
    let error = establish_reconnect(0, |_| {
        Ok(attempts
            .pop_front()
            .expect("exhaustion should consume two fresh snapshots"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("reconnect exhausted"));

    let live_lag = CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence.saturating_sub(1),
        oldest_available_sequence: cursor.last_event_sequence,
        fresh_cursor: cursor,
        reason: CodingAgentRecoveryReason::LiveReceiverLag,
        snapshot: Box::new(initial.session),
    };
    let (delivery_tx, delivery_rx) = mpsc::channel(1);
    let mut source = DesktopProductEventSource {
        replay: VecDeque::new(),
        receiver: DesktopProductEventReceiver::Injected(delivery_rx),
    };
    delivery_tx
        .send(Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(
            live_lag,
        )))
        .await
        .unwrap();
    let CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery) =
        source.recv().await.unwrap()
    else {
        panic!("injected live lag must reach the desktop recovery branch");
    };
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = recovery_update(recovery) else {
        panic!("live lag delivery must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_live_receiver_lag");

    bridge.shutdown().await.unwrap();
}
