use super::*;

pub(super) fn public_client_snapshot(state: ClientSnapshotState) -> CodingAgentSnapshot {
    let mut snapshot: CodingAgentSnapshot = state.snapshot.into();
    snapshot.drafts = state
        .drafts
        .into_iter()
        .map(|draft| CodingAgentDraft {
            id: CodingAgentDraftId(draft.id),
            kind: match draft.kind {
                ClientDraftKind::Prompt => CodingAgentDraftKind::Prompt,
                ClientDraftKind::Steer => CodingAgentDraftKind::Steer,
                ClientDraftKind::FollowUp => CodingAgentDraftKind::FollowUp,
            },
            text: draft.text,
        })
        .collect();
    snapshot.submitted_operation = state.submitted_operation.map(|submitted| match submitted {
        SubmittedOperationStatus::Running {
            operation_id, kind, ..
        } => CodingAgentSubmittedOperation {
            operation_id,
            kind: kind.as_str().into(),
            status: CodingAgentSubmittedOperationStatus::Running,
        },
        SubmittedOperationStatus::RecoveryPending {
            operation_id,
            kind,
            recovery_id,
            ..
        } => CodingAgentSubmittedOperation {
            operation_id,
            kind: kind.as_str().into(),
            status: CodingAgentSubmittedOperationStatus::RecoveryPending { recovery_id },
        },
        SubmittedOperationStatus::Terminal {
            operation_id,
            kind,
            anchor,
            status,
            ..
        } => CodingAgentSubmittedOperation {
            operation_id,
            kind: kind.as_str().into(),
            status: CodingAgentSubmittedOperationStatus::Terminal {
                status,
                anchor: match anchor {
                    crate::application::snapshot::SubmittedTerminalAnchor::ProductEvent {
                        sequence,
                        durability,
                    } => CodingAgentSubmittedTerminalAnchor::ProductEvent {
                        sequence,
                        durability: match durability {
                            crate::application::snapshot::SubmittedEventDurability::Durable => {
                                CodingAgentSubmittedEventDurability::Durable
                            }
                            crate::application::snapshot::SubmittedEventDurability::Uncertain => {
                                CodingAgentSubmittedEventDurability::Uncertain
                            }
                        },
                    },
                    crate::application::snapshot::SubmittedTerminalAnchor::OutcomeOnly {
                        acknowledgement,
                    } => CodingAgentSubmittedTerminalAnchor::OutcomeOnly { acknowledgement },
                    crate::application::snapshot::SubmittedTerminalAnchor::TerminalUncertain {
                        operation_id,
                    } => CodingAgentSubmittedTerminalAnchor::TerminalUncertain {
                        operation_id,
                        recovery: CodingAgentTerminalUncertainty::RecoveryRequired,
                    },
                },
            },
        },
    });
    snapshot
}

pub(super) fn registry_error(
    _id: &CodingAgentClientId,
    error: ClientRegistryError,
) -> CodingSessionError {
    match error {
        ClientRegistryError::Lifecycle(reason) => CodingSessionError::Lifecycle { reason },
        ClientRegistryError::ClientCapacityExceeded { limit } => {
            CodingSessionError::ClientCapacityExceeded { limit }
        }
        ClientRegistryError::SubmissionDraftMismatch => CodingSessionError::SubmissionDraftMismatch,
        other => CodingSessionError::Input {
            message: other.to_string(),
        },
    }
}

pub(super) fn submission_preparation_error(
    id: &CodingAgentClientId,
    error: ClientRegistryError,
) -> CodingSessionError {
    match error {
        ClientRegistryError::SubmittedOperationPending => {
            CodingSessionError::SubmissionPreparationBusy
        }
        other => registry_error(id, other),
    }
}

pub(super) fn validate_submission_draft(
    id: &CodingAgentDraftId,
    display_text: &str,
) -> Result<(), CodingSessionError> {
    if id.0.is_empty() || id.0.len() > crate::limits::MAX_CLIENT_DRAFT_ID_BYTES {
        return Err(CodingSessionError::Input {
            message: format!(
                "client draft id must contain 1..={} bytes",
                crate::limits::MAX_CLIENT_DRAFT_ID_BYTES
            ),
        });
    }
    if display_text.len() > crate::limits::MAX_CLIENT_DRAFT_TEXT_BYTES {
        return Err(CodingSessionError::Input {
            message: format!(
                "client draft exceeds the {} byte safety limit",
                crate::limits::MAX_CLIENT_DRAFT_TEXT_BYTES
            ),
        });
    }
    Ok(())
}

pub(crate) fn internal_client_id(id: &CodingAgentClientId) -> ClientConnectionId {
    ClientConnectionId::new(id.as_str())
}

pub(crate) fn public_client_connection(
    id: CodingAgentClientId,
    coordinator: Arc<SnapshotCoordinator>,
    event_service: EventService,
    authorization_service: crate::services::authorization::AuthorizationService,
    handle: ClientHandle,
    state: ClientSnapshotState,
) -> CodingAgentClientConnection {
    debug_assert_eq!(handle.id.as_str(), id.as_str());
    CodingAgentClientConnection {
        coordinator,
        event_service,
        authorization_service,
        client_id: id,
        generation: CodingAgentConnectionGeneration(handle.generation.0),
        snapshot: public_client_snapshot(state),
    }
}
