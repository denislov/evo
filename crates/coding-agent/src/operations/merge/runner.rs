use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{ChangeEntry, WorktreeRegistry, apply_merge_cancellable};

use crate::kernel::error::CodingSessionError;
use crate::services::event::EventService;
use crate::services::ports::ExtensionHostService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeOutcome {
    pub(crate) worktree_id: String,
    pub(crate) applied: usize,
    pub(crate) entries: Vec<ChangeEntry>,
}

pub(crate) async fn list_proposals(
    registry: &Arc<WorktreeRegistry>,
    parent_workspace_root: &Path,
    cancellation: CancellationToken,
) -> Result<Vec<crate::events::CodingAgentMergeProposal>, CodingSessionError> {
    let parent = std::path::absolute(parent_workspace_root).map_err(|error| {
        CodingSessionError::Resource {
            message: format!(
                "cannot resolve session workspace {}: {error}",
                parent_workspace_root.display()
            ),
        }
    })?;
    let registry = registry.clone();
    tokio::task::spawn_blocking(move || {
        let mut proposals = Vec::new();
        for record in registry
            .load_all()
            .map_err(|error| CodingSessionError::Resource {
                message: format!("cannot list merge proposals: {error}"),
            })?
        {
            if record.source != parent
                || record.lifecycle != workspace_runtime::api::WorkspaceLifecycle::MergePending
            {
                continue;
            }
            if cancellation.is_cancelled() {
                return Err(CodingSessionError::Cancelled);
            }
            let changeset = workspace_runtime::api::build_changeset_cancellable(
                &registry,
                &record.id,
                &cancellation,
            )
            .map_err(|error| CodingSessionError::Resource {
                message: format!("cannot review proposal {}: {error}", record.id),
            })?;
            proposals.push(
                workspace_runtime::api::MergeProposal {
                    worktree_id: record.id,
                    child_operation_id: record.owner_operation,
                    changeset,
                }
                .into(),
            );
        }
        Ok(proposals)
    })
    .await
    .map_err(|error| CodingSessionError::Session {
        message: format!("merge proposal worker failed: {error}"),
    })?
}

/// Merge a `MergePending` worktree into the current session's parent workspace.
///
/// The parent must be the worktree record's source (authorization scope), the
/// parent must still sit on the child's base revision, and no parent-side
/// change may overlap the child's changes. Every outcome publishes a merge
/// event; a failed merge leaves the parent and the record untouched so the
/// proposal can be retried or discarded. user hooks receive
/// `merge_proposed` / `merge_applied` Observe 事件。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn merge_worktree(
    events: &EventService,
    extension_host: &ExtensionHostService,
    registry: &Arc<WorktreeRegistry>,
    parent_workspace_root: &Path,
    operation_id: &str,
    session_id: &str,
    worktree_id: &str,
    cancellation: CancellationToken,
) -> Result<MergeOutcome, CodingSessionError> {
    let workspace_root = parent_workspace_root.to_string_lossy().into_owned();
    extension_host.submit_event(
        extension_host::api::ExtensionEventKind::MergeProposed,
        session_id,
        &workspace_root,
        extension_host::api::ExtensionEventPayload::MergeProposed {
            proposal_id: operation_id.to_owned(),
            child_worktree: worktree_id.to_owned(),
        },
    );
    let record = registry
        .load(worktree_id)
        .map_err(|error| CodingSessionError::Resource {
            message: format!("cannot load worktree {worktree_id}: {error}"),
        })?
        .ok_or_else(|| CodingSessionError::Input {
            message: format!("worktree {worktree_id} is not registered"),
        })?;
    verify_owner(&record, parent_workspace_root, worktree_id)?;
    drop(record);

    let registry = registry.clone();
    let worktree_id = worktree_id.to_owned();
    let operation_id = operation_id.to_owned();
    let registry_for_merge = registry.clone();
    let worktree_id_for_merge = worktree_id.clone();
    let merge_cancellation = cancellation.clone();
    let report = tokio::task::spawn_blocking(move || {
        apply_merge_cancellable(
            &registry_for_merge,
            &worktree_id_for_merge,
            &merge_cancellation,
        )
    })
        .await
        .map_err(|error| CodingSessionError::Session {
            message: format!("merge worker failed: {error}"),
        })?
        .map_err(|error| match error {
            workspace_runtime::api::MergeError::Conflict { paths } => {
                let paths = paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                events
                    .emit_merge_conflicted(&operation_id, &worktree_id, paths.clone())
                    .ok();
                CodingSessionError::Conflict {
                    message: format!(
                        "merge of {} conflicts with parent-side changes on {paths:?}",
                        worktree_id
                    ),
                }
            }
            workspace_runtime::api::MergeError::StaleParent {
                expected,
                actual,
            } => {
                events
                    .emit_merge_stale_parent(
                        &operation_id,
                        &worktree_id,
                        expected.clone(),
                        actual.clone(),
                    )
                    .ok();
                CodingSessionError::Stale {
                    message: format!(
                        "parent moved past child base revision for {worktree_id} (expected {expected:?}, found {actual:?}); refresh and retry"
                    ),
                }
            }
            workspace_runtime::api::MergeError::Cancelled => CodingSessionError::Cancelled,
            other => {
                events
                    .emit_merge_failed(&operation_id, &worktree_id, &CodingSessionError::Resource {
                        message: other.to_string(),
                    })
                    .ok();
                CodingSessionError::Resource {
                    message: format!("cannot merge {worktree_id}: {other}"),
                }
            }
        })?;

    events
        .emit_merge_applied(&operation_id, &worktree_id, report.applied)
        .map_err(|error| CodingSessionError::Session {
            message: format!("cannot publish merge event: {error}"),
        })?;
    extension_host.submit_event(
        extension_host::api::ExtensionEventKind::MergeApplied,
        session_id,
        &workspace_root,
        extension_host::api::ExtensionEventPayload::MergeApplied {
            proposal_id: operation_id.to_owned(),
            applied_entries: u32::try_from(report.applied).unwrap_or(u32::MAX),
        },
    );
    discard_after_merge(
        events,
        &registry,
        parent_workspace_root,
        &operation_id,
        &worktree_id,
    )?;
    Ok(MergeOutcome {
        worktree_id,
        applied: report.applied,
        entries: report.entries,
    })
}

/// Discard a `MergePending`/`Merged` worktree without merging it.
pub(crate) fn discard_worktree(
    events: &EventService,
    registry: &Arc<WorktreeRegistry>,
    parent_workspace_root: &Path,
    operation_id: &str,
    worktree_id: &str,
) -> Result<(), CodingSessionError> {
    let record = registry
        .load(worktree_id)
        .map_err(|error| CodingSessionError::Resource {
            message: format!("cannot load worktree {worktree_id}: {error}"),
        })?
        .ok_or_else(|| CodingSessionError::Input {
            message: format!("worktree {worktree_id} is not registered"),
        })?;
    verify_owner(&record, parent_workspace_root, worktree_id)?;
    registry
        .discard(worktree_id)
        .map_err(|error| CodingSessionError::Resource {
            message: format!("cannot discard worktree {worktree_id}: {error}"),
        })?;
    events
        .emit_merge_discarded(operation_id, worktree_id)
        .map_err(|error| CodingSessionError::Session {
            message: format!("cannot publish discard event: {error}"),
        })
}

fn verify_owner(
    record: &workspace_runtime::api::WorktreeRecord,
    parent_workspace_root: &Path,
    worktree_id: &str,
) -> Result<(), CodingSessionError> {
    let parent = std::path::absolute(parent_workspace_root).map_err(|error| {
        CodingSessionError::Resource {
            message: format!(
                "cannot resolve session workspace {}: {error}",
                parent_workspace_root.display()
            ),
        }
    })?;
    if record.source != parent {
        return Err(CodingSessionError::UnsupportedCapability {
            capability: format!(
                "worktree {worktree_id} belongs to {}, not the current session workspace {}",
                record.source.display(),
                parent.display()
            ),
        });
    }
    Ok(())
}

fn discard_after_merge(
    events: &EventService,
    registry: &Arc<WorktreeRegistry>,
    parent_workspace_root: &Path,
    operation_id: &str,
    worktree_id: &str,
) -> Result<(), CodingSessionError> {
    if let Err(error) = discard_worktree(
        events,
        registry,
        parent_workspace_root,
        operation_id,
        worktree_id,
    ) {
        events.emit_diagnostic(
            Some(operation_id.to_owned()),
            format!("merged worktree cleanup failed: {error}"),
        )?;
    }
    Ok(())
}
