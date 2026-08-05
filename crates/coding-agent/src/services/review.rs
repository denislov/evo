use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use change_tracker::{
    ChangeReceipt, ChangeSource, HunkTrackerHandle, HunkTrackerOptions, HunkTrackerSnapshot,
    HunkTrackingService, ReconcileState, TrackingContext, WatchOptions,
};
use workspace_runtime::api::{WorkspaceHandle, WorkspaceKind};

use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::error::CodingSessionError;
use crate::mutex::MutexExt;
use crate::runtime::client::connection::CodingAgentHunkChangeSnapshot;
use crate::runtime::client::context::UiFileChangeProjection;
use crate::runtime::client::context_fold::MAX_CONTEXT_CHANGES;
use crate::services::event::EventService;

struct ReviewTrackingOwner {
    _service: HunkTrackingService,
    _projection_task: tokio::task::JoinHandle<()>,
}

/// Session-lifetime owner for filesystem facts and their product projection.
pub(crate) struct ReviewService {
    project_root: PathBuf,
    snapshots: Arc<SnapshotCoordinator>,
    events: EventService,
    latest: Arc<Mutex<HunkTrackerSnapshot>>,
    owner: Mutex<Option<ReviewTrackingOwner>>,
}

impl std::fmt::Debug for ReviewService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviewService")
            .field("project_root", &"<project-root>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MutationTracking {
    handle: HunkTrackerHandle,
    latest: Arc<Mutex<HunkTrackerSnapshot>>,
    session_id: String,
    turn_id: String,
    operation_id: String,
}

impl ReviewService {
    pub(crate) fn new(
        project_root: PathBuf,
        snapshots: Arc<SnapshotCoordinator>,
        events: EventService,
    ) -> Self {
        Self {
            project_root,
            snapshots,
            events,
            latest: Arc::new(Mutex::new(empty_snapshot())),
            owner: Mutex::new(None),
        }
    }

    pub(crate) fn mutation_tracking(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        operation_id: impl Into<String>,
    ) -> Result<MutationTracking, CodingSessionError> {
        let handle = self.ensure_started()?;
        Ok(MutationTracking {
            handle,
            latest: Arc::clone(&self.latest),
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            operation_id: operation_id.into(),
        })
    }

    pub(crate) fn latest(&self) -> Result<HunkTrackerSnapshot, CodingSessionError> {
        Ok(self
            .latest
            .lock_resource("review tracker snapshot")?
            .clone())
    }

    pub(crate) fn tracker_handle(&self) -> Result<HunkTrackerHandle, CodingSessionError> {
        self.ensure_started()
    }

    pub(crate) fn refresh_latest(
        &self,
        handle: &HunkTrackerHandle,
    ) -> Result<(), CodingSessionError> {
        let receiver = handle.snapshots();
        let snapshot = receiver.borrow().clone();
        *self.latest.lock_resource("review tracker snapshot")? = snapshot;
        Ok(())
    }

    pub(crate) fn product_changes(
        &self,
    ) -> Result<
        Vec<crate::runtime::client::connection::CodingAgentFileChangeSnapshot>,
        CodingSessionError,
    > {
        Ok(project_changes(&self.latest()?)
            .into_iter()
            .map(Into::into)
            .collect())
    }

    fn ensure_started(&self) -> Result<HunkTrackerHandle, CodingSessionError> {
        let mut owner = self.owner.lock_resource("review tracker owner")?;
        if let Some(owner) = owner.as_ref() {
            return Ok(owner._service.handle());
        }
        let workspace = WorkspaceHandle::new(WorkspaceKind::Source, &self.project_root)
            .map_err(review_start_error)?;
        let service = HunkTrackingService::start(
            &workspace,
            WatchOptions::default(),
            HunkTrackerOptions::default(),
        )
        .map_err(review_start_error)?;
        let handle = service.handle();
        let mut receiver = service.snapshots();
        let latest = Arc::clone(&self.latest);
        let snapshots = Arc::clone(&self.snapshots);
        let events = self.events.clone();
        let projection_task = tokio::spawn(async move {
            loop {
                let snapshot = receiver.borrow_and_update().clone();
                if let Ok(mut current) = latest.lock_resource("review tracker snapshot") {
                    *current = snapshot.clone();
                }
                let changes = project_changes(&snapshot);
                if snapshots.replace_review_changes(changes.clone()).is_err() {
                    return;
                }
                let product_changes = changes.into_iter().map(Into::into).collect();
                if events.emit_review_changes(product_changes).is_err() {
                    return;
                }
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        });
        *owner = Some(ReviewTrackingOwner {
            _service: service,
            _projection_task: projection_task,
        });
        Ok(handle)
    }
}

impl MutationTracking {
    pub(crate) async fn record(
        &self,
        tool_call_id: &str,
        receipt: ChangeReceipt,
    ) -> Result<(), String> {
        self.handle
            .record_receipt(
                receipt,
                ChangeSource::AgentEdit,
                TrackingContext {
                    session_id: self.session_id.clone(),
                    turn_id: self.turn_id.clone(),
                    operation_id: self.operation_id.clone(),
                    tool_call_id: Some(tool_call_id.to_owned()),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let receiver = self.handle.snapshots();
        let snapshot = receiver.borrow().clone();
        *self
            .latest
            .lock_resource("review tracker snapshot")
            .map_err(|error| error.to_string())? = snapshot;
        Ok(())
    }
}

fn project_changes(snapshot: &HunkTrackerSnapshot) -> Vec<UiFileChangeProjection> {
    let mut changes = snapshot
        .files
        .iter()
        .map(|file| {
            let hunks = file
                .hunks
                .iter()
                .map(|hunk| CodingAgentHunkChangeSnapshot {
                    id: hunk.id.as_str().to_owned(),
                    source: source_name(hunk.source).into(),
                    old_start: hunk.range.old_start,
                    old_lines: hunk.range.old_lines,
                    new_start: hunk.range.new_start,
                    new_lines: hunk.range.new_lines,
                    diff: hunk.unified_diff.clone(),
                })
                .collect::<Vec<_>>();
            let context = file.context.as_ref();
            let stats = diff_stats(&file.hunks);
            UiFileChangeProjection {
                path: display_path(&file.path),
                mutation_kind: file.mutation_kind.clone(),
                source: source_name(file.source).into(),
                operation_id: context
                    .map(|context| context.operation_id.clone())
                    .unwrap_or_else(|| "external".into()),
                tool_call_id: context.and_then(|context| context.tool_call_id.clone()),
                session_id: context.map(|context| context.session_id.clone()),
                turn_id: context.map(|context| context.turn_id.clone()),
                updated_sequence: file.recorded_sequence,
                before_revision: file.before_revision.clone(),
                after_revision: file.after_revision.clone(),
                after_exists: file.after_exists,
                first_changed_line: file
                    .hunks
                    .iter()
                    .filter_map(|hunk| (hunk.range.new_start > 0).then_some(hunk.range.new_start))
                    .min(),
                added_lines: stats.map(|(added, _)| added),
                removed_lines: stats.map(|(_, removed)| removed),
                diff: file_diff(&file.path, &file.hunks),
                hunks,
            }
        })
        .collect::<Vec<_>>();
    changes.sort_by(|left, right| {
        right
            .updated_sequence
            .cmp(&left.updated_sequence)
            .then_with(|| left.path.cmp(&right.path))
    });
    changes.truncate(MAX_CONTEXT_CHANGES);
    changes
}

fn file_diff(path: &Path, hunks: &[change_tracker::HunkSnapshot]) -> Option<String> {
    let bodies = hunks
        .iter()
        .map(|hunk| hunk.unified_diff.as_deref())
        .collect::<Option<Vec<_>>>()?;
    let path = display_path(path);
    let mut diff = format!("--- {path}\n+++ {path}");
    for body in bodies {
        diff.push('\n');
        diff.push_str(body);
    }
    Some(diff)
}

fn diff_stats(hunks: &[change_tracker::HunkSnapshot]) -> Option<(usize, usize)> {
    let diff = hunks
        .iter()
        .map(|hunk| hunk.unified_diff.as_deref())
        .collect::<Option<Vec<_>>>()?;
    Some(
        diff.into_iter()
            .flat_map(str::lines)
            .fold((0, 0), |(added, removed), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    (added.saturating_add(1), removed)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    (added, removed.saturating_add(1))
                } else {
                    (added, removed)
                }
            }),
    )
}

fn source_name(source: ChangeSource) -> &'static str {
    match source {
        ChangeSource::AgentEdit => "agent_edit",
        ChangeSource::ExternalEditOnAgentFile => "external_edit_on_agent_file",
        ChangeSource::ExternalEdit => "external_edit",
        ChangeSource::MergeApply => "merge_apply",
        ChangeSource::HookEdit => "hook_edit",
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn empty_snapshot() -> HunkTrackerSnapshot {
    HunkTrackerSnapshot {
        files: Vec::new(),
        facts: Vec::new(),
        reconcile: ReconcileState::Ready,
        pending_receipts: 0,
        pending_events: 0,
    }
}

fn review_start_error(error: impl std::fmt::Display) -> CodingSessionError {
    CodingSessionError::Resource {
        message: format!("cannot start review tracking: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::*;
    use crate::events::{CodingAgentProductEventKind, CodingAgentReviewProductEvent};
    use crate::services::event::EventService;

    fn revision(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn typed_receipt_updates_snapshot_and_live_review_projection() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
        let snapshots = SnapshotCoordinator::new();
        let events = EventService::with_snapshot_coordinator(snapshots.clone());
        let mut receiver = events.subscribe_product_events();
        let service = ReviewService::new(dir.path().to_path_buf(), snapshots.clone(), events);
        let tracking = service
            .mutation_tracking("session-1", "turn-1", "operation-1")
            .unwrap();
        tracking
            .record(
                "tool-1",
                ChangeReceipt {
                    path: "notes.txt".into(),
                    target_fingerprint: "target:notes.txt".into(),
                    before_revision: Some(revision(b"before\n")),
                    after_revision: revision(b"after\n"),
                    after_exists: true,
                    byte_delta: 0,
                    line_delta: 0,
                    origin: "edit".into(),
                    unified_diff: Some(
                        "--- notes.txt\n+++ notes.txt\n@@ -1,1 +1,1 @@\n-before\n+after".into(),
                    ),
                },
            )
            .await
            .unwrap();

        let changes = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = receiver.recv().await.unwrap();
                if let CodingAgentProductEventKind::Review(CodingAgentReviewProductEvent::Changed {
                    changes,
                }) = event.event()
                    && !changes.is_empty()
                {
                    break changes.clone();
                }
            }
        })
        .await
        .expect("review projection event arrives");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "notes.txt");
        assert_eq!(changes[0].source, "agent_edit");
        assert_eq!(changes[0].added_lines, Some(1));
        assert_eq!(changes[0].removed_lines, Some(1));
        assert_eq!(changes[0].hunks.len(), 1);
        assert_eq!(
            snapshots
                .state
                .lock_resource("review test snapshot")
                .unwrap()
                .context_projection
                .changes
                .len(),
            1
        );
    }
}
