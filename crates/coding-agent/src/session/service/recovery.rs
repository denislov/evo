use super::*;

impl SessionService {
    pub(crate) async fn retry_recovery(
        &self,
        request: &CodingAgentRecoveryRetryRequest,
    ) -> Result<RecoveryRetryCommit, CodingSessionError> {
        let pending = self
            .inspect_recovery_pending()?
            .into_iter()
            .find(|pending| pending.recovery_id == request.recovery_id)
            .ok_or_else(|| CodingSessionError::Input {
                message: format!(
                    "unknown or already resolved recovery: {}",
                    request.recovery_id
                ),
            })?;
        if pending.operation_id != request.operation_id {
            return Err(CodingSessionError::Input {
                message: "recovery operation identity mismatch".into(),
            });
        }
        if pending.record_version != request.expected_record_version {
            return Err(CodingSessionError::Input {
                message: "recovery record version is stale".into(),
            });
        }
        if pending.descriptor_revision != request.expected_descriptor_revision {
            return Err(CodingSessionError::Input {
                message: "recovery descriptor revision is stale".into(),
            });
        }
        if pending.capability_generation != request.expected_capability_generation {
            return Err(CodingSessionError::Input {
                message: "recovery capability generation is stale".into(),
            });
        }
        if pending.attempt_count != request.expected_attempt_count {
            return Err(CodingSessionError::Input {
                message: "recovery attempt count is stale".into(),
            });
        }
        if pending.attempt_count >= MAX_RECOVERY_RETRY_ATTEMPTS {
            return Err(CodingSessionError::Input {
                message: format!("recovery retry limit reached: {MAX_RECOVERY_RETRY_ATTEMPTS}"),
            });
        }
        let operation_kind = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .find_map(|event| match event.data {
                SessionEventData::OperationStarted { operation, .. }
                    if event.operation_id.as_deref() == Some(request.operation_id.as_str()) =>
                {
                    Some(operation)
                }
                _ => None,
            })
            .ok_or_else(|| CodingSessionError::Session {
                message: "recovery retry requires the original operation kind".into(),
            })?;
        if matches!(
            operation_kind,
            crate::session::event::OperationKind::Other { .. }
                | crate::session::event::OperationKind::SessionTreeLabel
        ) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery retry requires a durable root operation family".into(),
            });
        }
        let session_id = self.session_id().to_owned();
        let last_attempt_at = SystemClock.now_rfc3339();
        let attempt_count = pending.attempt_count + 1;
        let reason = if request.schedule_with_backoff {
            "recovery retry scheduled deterministic backoff after durable inspection"
        } else {
            "recovery retry inspected durable facts and outbox; operation remains pending"
        };
        let next_attempt_at = request
            .schedule_with_backoff
            .then(|| recovery_next_attempt_at(&last_attempt_at, attempt_count))
            .transpose()?;
        let mut ids = SystemIdGenerator;
        let event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            last_attempt_at.clone(),
            SessionEventData::OperationRecoveryPending {
                reason: reason.into(),
                recovery_id: pending.recovery_id.clone(),
                record_version: pending.record_version,
                descriptor_revision: pending.descriptor_revision,
                capability_generation: pending.capability_generation,
                attempt_count,
                last_attempt_at: Some(last_attempt_at.clone()),
                next_attempt_at: next_attempt_at.clone(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let draft = crate::events::recovery::RecoveryPendingEvent {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            reason: reason.into(),
            session_id: session_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
            attempt_count,
            last_attempt_at: Some(last_attempt_at.clone()),
            next_attempt_at: next_attempt_at.clone(),
        }
        .into_product_draft();
        let outbox = DurableOutboxRecordCandidate::new(
            format!(
                "{}/{}/recovery_pending/retry/{}",
                session_id, request.operation_id, attempt_count
            ),
            session_id,
            Some(request.operation_id.clone()),
            vec![event.event_id.clone()],
            DurableOutboxRecordKind::Recovery,
            draft.clone(),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?;
        self.commit_writer_mutation_with_outbox(
            vec![event],
            vec![outbox],
            ManifestPatch::new().updated_at(last_attempt_at.clone()),
            Some(request.operation_id.clone()),
        )
        .await?;
        Ok(RecoveryRetryCommit {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id,
            operation_kind,
            capability_generation: pending.capability_generation,
            draft,
            attempt_count,
            last_attempt_at,
            next_attempt_at,
        })
    }

    pub(super) fn apply_startup_recovery(&mut self) -> Result<(), CodingSessionError> {
        let replay = self.replay()?;
        let in_doubt_operations = replay.recovery_summary().in_doubt_operations;
        let pending_tool_authorizations = replay.pending_tool_authorizations;
        if in_doubt_operations.is_empty() && pending_tool_authorizations.is_empty() {
            return Ok(());
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let observed_at = clock.now_rfc3339();
        let reason =
            "startup recovery retained incomplete operation as recovery-pending".to_owned();
        let authorization_reason =
            "startup recovery interrupted unresolved tool authorization".to_owned();
        let durable_events = self.store.read_events(&self.handle)?;
        let operation_facts = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationStarted {
                    operation,
                    runtime_generation,
                } => event.operation_id.clone().map(|operation_id| {
                    (
                        operation_id,
                        (operation.clone(), runtime_generation.capability_generation),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let existing_pending = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationRecoveryPending {
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                    ..
                } => event.operation_id.clone().map(|operation_id| {
                    (
                        operation_id,
                        (
                            recovery_id.clone(),
                            *record_version,
                            *descriptor_revision,
                            *capability_generation,
                            *attempt_count,
                            last_attempt_at.clone(),
                            next_attempt_at.clone(),
                        ),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let markers = in_doubt_operations
            .into_iter()
            .filter(|operation_id| !existing_pending.contains_key(operation_id))
            .map(|operation_id| {
                let recovery_id = format!("recovery_pending:{session_id}/{operation_id}");
                let (operation_kind, capability_generation) = operation_facts
                    .get(&operation_id)
                    .cloned()
                    .map(|(kind, generation)| (Some(kind), generation))
                    .unwrap_or((None, None));
                StartupRecoveryMarker {
                    operation_id,
                    recovery_id,
                    reason: reason.clone(),
                    session_id: session_id.clone(),
                    operation_kind,
                    capability_generation,
                    record_version: RECOVERY_RECORD_VERSION,
                    descriptor_revision: crate::kernel::operation::OPERATION_DESCRIPTOR_REVISION,
                    attempt_count: 0,
                    last_attempt_at: None,
                    next_attempt_at: None,
                }
            })
            .collect::<Vec<_>>();
        let mut retry_markers = existing_pending
            .iter()
            .filter_map(|(operation_id, pending)| {
                let next_attempt_at = pending.6.as_deref()?;
                if pending.4 >= MAX_RECOVERY_RETRY_ATTEMPTS
                    || !recovery_retry_is_due(&observed_at, next_attempt_at)
                {
                    return None;
                }
                let (operation_kind, _) = operation_facts.get(operation_id).cloned().unwrap_or((
                    OperationKind::Other {
                        name: "unknown".into(),
                    },
                    None,
                ));
                Some(StartupRecoveryMarker {
                    operation_id: operation_id.clone(),
                    recovery_id: pending.0.clone(),
                    reason: "automatic recovery retry inspected durable facts and outbox"
                        .to_owned(),
                    session_id: session_id.clone(),
                    operation_kind: Some(operation_kind),
                    capability_generation: pending.3,
                    record_version: pending.1,
                    descriptor_revision: pending.2,
                    attempt_count: pending.4 + 1,
                    last_attempt_at: Some(observed_at.clone()),
                    next_attempt_at: None,
                })
            })
            .collect::<Vec<_>>();
        let mut all_markers = markers;
        all_markers.append(&mut retry_markers);
        let recovery_events = all_markers
            .iter()
            .map(|marker| {
                SessionEventEnvelope::new(
                    session_id.clone(),
                    ids.next_event_id(),
                    observed_at.clone(),
                    SessionEventData::OperationRecoveryPending {
                        reason: marker.reason.clone(),
                        recovery_id: marker.recovery_id.clone(),
                        record_version: marker.record_version,
                        descriptor_revision: marker.descriptor_revision,
                        capability_generation: marker.capability_generation,
                        attempt_count: marker.attempt_count,
                        last_attempt_at: marker.last_attempt_at.clone(),
                        next_attempt_at: marker.next_attempt_at.clone(),
                    },
                )
                .with_operation_id(marker.operation_id.clone())
            })
            .collect::<Vec<_>>();
        let recovery_outbox = all_markers
            .iter()
            .zip(&recovery_events)
            .map(|(marker, event)| {
                DurableOutboxRecordCandidate::new(
                    if marker.attempt_count == 0 {
                        format!(
                            "{}/{}/recovery_pending",
                            marker.session_id, marker.operation_id
                        )
                    } else {
                        format!(
                            "{}/{}/recovery_pending/retry/{}",
                            marker.session_id, marker.operation_id, marker.attempt_count
                        )
                    },
                    marker.session_id.clone(),
                    Some(marker.operation_id.clone()),
                    vec![event.event_id.clone()],
                    DurableOutboxRecordKind::Recovery,
                    crate::events::recovery::RecoveryPendingEvent {
                        operation_id: marker.operation_id.clone(),
                        recovery_id: marker.recovery_id.clone(),
                        reason: marker.reason.clone(),
                        session_id: marker.session_id.clone(),
                        record_version: marker.record_version,
                        descriptor_revision: marker.descriptor_revision,
                        capability_generation: marker.capability_generation,
                        attempt_count: marker.attempt_count,
                        last_attempt_at: marker.last_attempt_at.clone(),
                        next_attempt_at: marker.next_attempt_at.clone(),
                    }
                    .into_product_draft(),
                )
                .map_err(|message| CodingSessionError::Session {
                    message: message.into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut events = recovery_events;
        events.extend(pending_tool_authorizations.into_iter().map(|request| {
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                observed_at.clone(),
                SessionEventData::ToolAuthorizationResolved {
                    authorization_id: request.authorization_id,
                    resolution: PersistedToolAuthorizationResolution::Interrupted {
                        reason: authorization_reason.clone(),
                    },
                },
            )
            .with_operation_id(request.operation_id)
            .with_turn_id(request.turn_id)
        }));

        if events.is_empty() {
            return Ok(());
        }

        self.commit_writer_mutation_with_outbox_blocking(
            events,
            recovery_outbox,
            ManifestPatch::new().updated_at(observed_at),
            None,
        )?;
        self.startup_recovery_markers.extend(all_markers);
        Ok(())
    }
}

fn recovery_next_attempt_at(
    last_attempt_at: &str,
    attempt_count: u32,
) -> Result<String, CodingSessionError> {
    let seconds = 1_i64 << attempt_count.saturating_sub(1).min(2);
    let timestamp = time::OffsetDateTime::parse(
        last_attempt_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|error| CodingSessionError::Session {
        message: format!("recovery retry timestamp is invalid: {error}"),
    })?
    .checked_add(time::Duration::seconds(seconds))
    .ok_or_else(|| CodingSessionError::Session {
        message: "recovery retry timestamp overflow".into(),
    })?;
    timestamp
        .format(&time::format_description::well_known::Rfc3339)
        .map(|value| value.replace("+00:00", "Z"))
        .map_err(|error| CodingSessionError::Session {
            message: format!("recovery retry timestamp formatting failed: {error}"),
        })
}

fn recovery_retry_is_due(now: &str, next_attempt_at: &str) -> bool {
    let format = &time::format_description::well_known::Rfc3339;
    match (
        time::OffsetDateTime::parse(now, format),
        time::OffsetDateTime::parse(next_attempt_at, format),
    ) {
        (Ok(now), Ok(next)) => next <= now,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_backoff_transition_table() {
        let cases = [
            (0, "2026-08-02T00:00:01Z"),
            (1, "2026-08-02T00:00:01Z"),
            (2, "2026-08-02T00:00:02Z"),
            (3, "2026-08-02T00:00:04Z"),
            (4, "2026-08-02T00:00:04Z"),
            (u32::MAX, "2026-08-02T00:00:04Z"),
        ];

        for (attempt_count, expected) in cases {
            assert_eq!(
                recovery_next_attempt_at("2026-08-02T00:00:00Z", attempt_count).unwrap(),
                expected,
                "attempt {attempt_count}"
            );
        }
    }

    #[test]
    fn recovery_retry_due_transition_table() {
        let cases = [
            (
                "before deadline",
                "2026-08-02T00:00:00Z",
                "2026-08-02T00:00:01Z",
                false,
            ),
            (
                "at deadline",
                "2026-08-02T00:00:01Z",
                "2026-08-02T00:00:01Z",
                true,
            ),
            (
                "after deadline",
                "2026-08-02T00:00:02Z",
                "2026-08-02T00:00:01Z",
                true,
            ),
            (
                "invalid observation",
                "not-a-timestamp",
                "2026-08-02T00:00:01Z",
                false,
            ),
            (
                "invalid deadline",
                "2026-08-02T00:00:01Z",
                "not-a-timestamp",
                false,
            ),
        ];

        for (name, now, next_attempt_at, expected) in cases {
            assert_eq!(
                recovery_retry_is_due(now, next_attempt_at),
                expected,
                "{name}"
            );
        }
    }
}
