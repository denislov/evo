use crate::runtime::client::connection::{
    CodingAgentContextSnapshot, CodingAgentDelegationSnapshot, CodingAgentDraft,
    CodingAgentDraftId, CodingAgentDraftKind, CodingAgentFileChangeSnapshot,
    CodingAgentOperationSnapshot, CodingAgentOperationStatus, CodingAgentSnapshot,
    CodingAgentSnapshotCursor, CodingAgentTurnUsageSnapshot, CodingAgentUsageSnapshot,
};
use crate::runtime::client::context::{
    UiContextProjection, UiDelegationProjection, UiFileChangeProjection, UiOperationProjection,
    UiOperationStatus, UiTurnUsageProjection, UiUsageProjection,
};
use crate::runtime::client::state::UiSnapshot;
use crate::runtime::version::UI_SNAPSHOT_PROTOCOL_VERSION;

impl From<UiSnapshot> for CodingAgentSnapshot {
    fn from(snapshot: UiSnapshot) -> Self {
        Self {
            cursor: CodingAgentSnapshotCursor {
                stream_id: snapshot.cursor.stream_id.clone(),
                snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
                last_event_sequence: snapshot.cursor.last_event_sequence.get(),
                last_session_sequence: snapshot.cursor.last_session_sequence,
                capability_generation: snapshot.cursor.capability_generation.get(),
            },
            version: snapshot.version,
            session: snapshot.session,
            capabilities: snapshot.capabilities,
            active_operation: snapshot
                .active_operation
                .map(|kind| kind.as_str().to_owned()),
            pending_authorizations: snapshot.pending_authorizations,
            context: snapshot.context.into(),
            drafts: snapshot
                .client_drafts
                .into_iter()
                .enumerate()
                .map(|(index, draft)| CodingAgentDraft {
                    id: CodingAgentDraftId(index.to_string()),
                    kind: match draft.kind {
                        crate::runtime::client::state::ClientDraftKind::Prompt => {
                            CodingAgentDraftKind::Prompt
                        }
                        crate::runtime::client::state::ClientDraftKind::Steer => {
                            CodingAgentDraftKind::Steer
                        }
                        crate::runtime::client::state::ClientDraftKind::FollowUp => {
                            CodingAgentDraftKind::FollowUp
                        }
                    },
                    text: draft.text,
                })
                .collect(),
            submitted_operation: None,
        }
    }
}

impl From<UiOperationStatus> for CodingAgentOperationStatus {
    fn from(status: UiOperationStatus) -> Self {
        match status {
            UiOperationStatus::Running => Self::Running,
            UiOperationStatus::Completed => Self::Completed,
            UiOperationStatus::Failed => Self::Failed,
            UiOperationStatus::Aborted => Self::Aborted,
            UiOperationStatus::Recovered => Self::Recovered,
        }
    }
}

impl From<UiOperationProjection> for CodingAgentOperationSnapshot {
    fn from(operation: UiOperationProjection) -> Self {
        Self {
            operation_id: operation.operation_id,
            kind: operation.kind,
            parent_operation_id: operation.parent_operation_id,
            root_operation_id: operation.root_operation_id,
            status: operation.status.into(),
            started_sequence: operation.started_sequence,
            updated_sequence: operation.updated_sequence,
            diagnostics: operation.diagnostics,
            failure: operation.failure,
        }
    }
}

impl From<UiFileChangeProjection> for CodingAgentFileChangeSnapshot {
    fn from(change: UiFileChangeProjection) -> Self {
        Self {
            path: change.path,
            mutation_kind: change.mutation_kind,
            source: change.source,
            operation_id: change.operation_id,
            tool_call_id: change.tool_call_id,
            session_id: change.session_id,
            turn_id: change.turn_id,
            updated_sequence: change.updated_sequence,
            before_revision: change.before_revision,
            after_revision: change.after_revision,
            after_exists: change.after_exists,
            first_changed_line: change.first_changed_line,
            added_lines: change.added_lines,
            removed_lines: change.removed_lines,
            diff: change.diff,
            hunks: change.hunks,
        }
    }
}

impl From<UiDelegationProjection> for CodingAgentDelegationSnapshot {
    fn from(delegation: UiDelegationProjection) -> Self {
        Self {
            tool_call_id: delegation.tool_call_id,
            child_operation_id: delegation.child_operation_id,
            target_kind: delegation.target_kind,
            target_id: delegation.target_id,
            task: delegation.task,
            status: delegation.status,
            updated_sequence: delegation.updated_sequence,
            summary: delegation.summary,
            failure: delegation.failure,
        }
    }
}

impl From<UiTurnUsageProjection> for CodingAgentTurnUsageSnapshot {
    fn from(usage: UiTurnUsageProjection) -> Self {
        Self {
            turn_id: usage.turn_id,
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            context_tokens: usage.context_tokens,
            cost: usage.cost,
        }
    }
}

impl From<UiUsageProjection> for CodingAgentUsageSnapshot {
    fn from(usage: UiUsageProjection) -> Self {
        Self {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            cost: usage.cost,
            latest_turn: usage.latest_turn.map(Into::into),
            model_id: usage.model_id,
            context_window: usage.context_window,
        }
    }
}

impl From<UiContextProjection> for CodingAgentContextSnapshot {
    fn from(context: UiContextProjection) -> Self {
        Self {
            operations: context.operations.into_iter().map(Into::into).collect(),
            changes: context.changes.into_iter().map(Into::into).collect(),
            delegations: context.delegations.into_iter().map(Into::into).collect(),
            usage: context.usage.into(),
        }
    }
}
