use std::time::Instant;

use super::WorktreeRecord;

pub(super) fn creating(record: &WorktreeRecord, owner_operation: &str) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase = "creating",
        worktree_id = record.id.as_str(),
        owner_operation,
        creation_mode = ?record.creation_mode,
    );
}

pub(super) fn terminal(
    phase: &'static str,
    worktree_id: &str,
    owner_operation: &str,
    started: Instant,
) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase,
        worktree_id,
        owner_operation,
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

pub(super) fn ready(record: &WorktreeRecord, owner_operation: &str, started: Instant) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase = "ready",
        worktree_id = record.id.as_str(),
        owner_operation,
        creation_mode = ?record.creation_mode,
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

pub(super) fn discarding(worktree_id: &str) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase = "discarding",
        worktree_id,
    );
}

pub(super) fn discarded(worktree_id: &str, started: Instant, succeeded: bool) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase = if succeeded { "removed" } else { "failed" },
        worktree_id,
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

pub(super) fn transitioned(record: &WorktreeRecord) {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "worktree",
        phase = "transitioned",
        worktree_id = record.id.as_str(),
        state = ?record.lifecycle,
    );
}
