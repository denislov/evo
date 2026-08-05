use super::{HunkTrackerOptions, TrackingContext};
use crate::ChangeTrackerError;

pub(super) fn validate_options(options: &HunkTrackerOptions) -> Result<(), ChangeTrackerError> {
    let invalid = [
        (
            options.causal_window.is_zero(),
            "causal_window must be non-zero",
        ),
        (options.command_queue == 0, "command_queue must be non-zero"),
        (
            options.max_pending_facts == 0,
            "max_pending_facts must be non-zero",
        ),
        (
            options.max_change_facts == 0,
            "max_change_facts must be non-zero",
        ),
        (options.max_files == 0, "max_files must be non-zero"),
        (
            options.max_hunks_per_file == 0,
            "max_hunks_per_file must be non-zero",
        ),
        (
            options.max_diff_bytes == 0,
            "max_diff_bytes must be non-zero",
        ),
        (
            options.max_diff_lines == 0,
            "max_diff_lines must be non-zero",
        ),
        (
            options.max_history_bytes == 0,
            "max_history_bytes must be non-zero",
        ),
        (
            options.max_content_bytes == 0,
            "max_content_bytes must be non-zero",
        ),
    ];
    if let Some((_, message)) = invalid.into_iter().find(|(invalid, _)| *invalid) {
        return Err(ChangeTrackerError::InvalidOptions {
            message: message.into(),
        });
    }
    Ok(())
}

pub(super) fn validate_context(context: &TrackingContext) -> Result<(), ChangeTrackerError> {
    if context.session_id.is_empty()
        || context.turn_id.is_empty()
        || context.operation_id.is_empty()
        || context.tool_call_id.as_ref().is_some_and(String::is_empty)
    {
        return Err(ChangeTrackerError::InvalidFact {
            message: "tracking context requires session_id, turn_id, and operation_id".into(),
        });
    }
    Ok(())
}

pub(super) fn validate_revision(revision: &str, field: &str) -> Result<(), ChangeTrackerError> {
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ChangeTrackerError::InvalidFact {
            message: format!("{field} must be a SHA-256 content revision"),
        });
    }
    Ok(())
}
