use std::path::Path;
use std::sync::{Arc, Mutex};

use change_tracker::{
    ChangeReceipt, ChangeSource, HunkTrackerCheckpoint, HunkTrackerHandle, HunkTrackerOptions,
    HunkTrackerSnapshot, HunkTrackingService, ReconcileState, TrackingContext, WatchOptions,
};
use sha2::Digest;
use workspace_runtime::api::{
    WorkspaceAccessHandle, WorkspaceFileSnapshot, WorkspaceRestoreEntry, WorkspaceRestoreError,
    WorkspaceRestorePlan, WorkspaceSnapshot, WorkspaceSnapshotError, capture_workspace_snapshot,
    restore_workspace_snapshot,
};
#[cfg(test)]
use workspace_runtime::api::{WorkspaceHandle, WorkspaceKind};

use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::error::CodingSessionError;
use crate::mutex::MutexExt;
use crate::runtime::client::connection::CodingAgentHunkChangeSnapshot;
use crate::runtime::client::context::UiFileChangeProjection;
use crate::runtime::client::context_fold::MAX_CONTEXT_CHANGES;
use crate::services::event::EventService;

struct ReviewTrackingOwner {
    service: HunkTrackingService,
    projection_task: tokio::task::JoinHandle<()>,
}

/// Session-lifetime owner for filesystem facts and their product projection.
pub(crate) struct ReviewService {
    workspace: WorkspaceAccessHandle,
    snapshots: Arc<SnapshotCoordinator>,
    events: EventService,
    latest: Arc<Mutex<HunkTrackerSnapshot>>,
    owner: Mutex<Option<ReviewTrackingOwner>>,
    /// ARC-730：hook 修改归因共享的 tracker handle 槽。tracker 启动时
    /// 填充、停用时清空 —— 观察点不长期持有 handle（否则 watch channel
    /// 永不关闭，rewind 的 projection task join 会悬挂）。
    hook_tracker_slot: Mutex<Option<Arc<Mutex<Option<HunkTrackerHandle>>>>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewCheckpoint {
    pub(crate) tracker: HunkTrackerCheckpoint,
    pub(crate) workspace: WorkspaceSnapshot,
}

impl ReviewService {
    pub(crate) fn new(
        workspace: WorkspaceAccessHandle,
        snapshots: Arc<SnapshotCoordinator>,
        events: EventService,
    ) -> Self {
        Self {
            workspace,
            snapshots,
            events,
            latest: Arc::new(Mutex::new(empty_snapshot())),
            owner: Mutex::new(None),
            hook_tracker_slot: Mutex::new(None),
        }
    }

    /// 共享 tracker handle 槽（ARC-730）：hook 修改归因观察点从槽读取
    /// 当前 handle；tracker 生命周期由本 service 维护（启动填充 / 停用
    /// 清空），观察点不长期持有。
    pub(crate) fn bind_hook_tracker_slot(&self, slot: Arc<Mutex<Option<HunkTrackerHandle>>>) {
        *self
            .hook_tracker_slot
            .lock_or_recover("review hook tracker slot") = Some(slot);
    }

    /// 把当前 tracker handle 写入归因槽（`None` = tracker 已停用）。
    fn set_hook_tracker(&self, handle: Option<HunkTrackerHandle>) {
        if let Some(slot) = self
            .hook_tracker_slot
            .lock_or_recover("review hook tracker slot")
            .as_ref()
        {
            *slot.lock_or_recover("review hook tracker slot") = handle;
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

    pub(crate) async fn checkpoint(&self) -> Result<ReviewCheckpoint, CodingSessionError> {
        let tracker = self
            .ensure_started()?
            .checkpoint()
            .await
            .map_err(review_tracker_error)?;
        let workspace = capture_workspace_snapshot(&self.workspace)
            .await
            .map_err(review_snapshot_error)?;
        validate_tracker_workspace(&tracker, &workspace)?;
        Ok(ReviewCheckpoint { tracker, workspace })
    }

    pub(crate) async fn restore_checkpoint(
        &self,
        checkpoint: &ReviewCheckpoint,
    ) -> Result<(), CodingSessionError> {
        let workspace = capture_workspace_snapshot(&self.workspace)
            .await
            .map_err(review_snapshot_error)?;
        if workspace != checkpoint.workspace {
            return Err(CodingSessionError::Stale {
                message: "workspace no longer matches the rewind checkpoint".into(),
            });
        }
        let handle = self.ensure_started()?;
        handle
            .restore_checkpoint(checkpoint.tracker.clone())
            .await
            .map_err(review_tracker_error)?;
        self.refresh_latest(&handle)
    }

    pub(crate) async fn restore_workspace_and_tracker(
        &self,
        current: &ReviewCheckpoint,
        target: &ReviewCheckpoint,
        operation_id: &str,
    ) -> Result<(), CodingSessionError> {
        self.stop_tracking().await?;
        let plan = workspace_restore_plan(&current.workspace, &target.workspace);
        if let Err(error) = restore_workspace_snapshot(&self.workspace, plan).await {
            let mapped = review_restore_error(error, operation_id);
            if matches!(mapped, CodingSessionError::PartialCommit { .. }) {
                return Err(mapped);
            }
            if let Err(restart_error) = self.restore_checkpoint(current).await {
                return Err(CodingSessionError::PartialCommit {
                    operation_id: operation_id.to_owned(),
                    message: format!(
                        "workspace rewind failed: {mapped}; tracker restart failed: {restart_error}"
                    ),
                });
            }
            return Err(mapped);
        }
        if let Err(error) = self.restore_checkpoint(target).await {
            self.stop_tracking().await?;
            let rollback = restore_workspace_snapshot(
                &self.workspace,
                workspace_restore_plan(&target.workspace, &current.workspace),
            )
            .await
            .map(|_| ());
            if let Err(rollback_error) = rollback {
                return Err(CodingSessionError::PartialCommit {
                    operation_id: operation_id.to_owned(),
                    message: format!(
                        "tracker restore failed: {error}; workspace rollback failed: {rollback_error}"
                    ),
                });
            }
            if let Err(restart_error) = self.restore_checkpoint(current).await {
                return Err(CodingSessionError::PartialCommit {
                    operation_id: operation_id.to_owned(),
                    message: format!(
                        "tracker restore failed: {error}; tracker rollback failed: {restart_error}"
                    ),
                });
            }
            return Err(error);
        }
        Ok(())
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
            return Ok(owner.service.handle());
        }
        let service = HunkTrackingService::start(
            self.workspace.identity(),
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
            service,
            projection_task,
        });
        // ARC-730：把当前 handle 同步进 hook 归因槽。
        self.set_hook_tracker(Some(handle.clone()));
        Ok(handle)
    }

    async fn stop_tracking(&self) -> Result<(), CodingSessionError> {
        let owner = self.owner.lock_resource("review tracker owner")?.take();
        let Some(ReviewTrackingOwner {
            service,
            projection_task,
        }) = owner
        else {
            return Ok(());
        };
        // ARC-730：tracker 停用前清空归因槽（handle 随 service 释放，
        // watch channel 关闭后 projection task 正常退出）。
        self.set_hook_tracker(None);
        service.shutdown().await.map_err(review_tracker_error)?;
        projection_task
            .await
            .map_err(|error| CodingSessionError::Resource {
                message: format!("review projection task failed during rewind: {error}"),
            })?;
        Ok(())
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

fn review_tracker_error(error: change_tracker::ChangeTrackerError) -> CodingSessionError {
    CodingSessionError::Resource {
        message: format!("review tracker checkpoint failed: {error}"),
    }
}

fn review_snapshot_error(error: WorkspaceSnapshotError) -> CodingSessionError {
    match error {
        WorkspaceSnapshotError::UnsupportedWorkspace { kind } => {
            CodingSessionError::UnsupportedCapability {
                capability: format!("workspace kind {kind:?} does not support rewind"),
            }
        }
        WorkspaceSnapshotError::Capture { .. }
        | WorkspaceSnapshotError::Invalid { .. }
        | WorkspaceSnapshotError::BudgetExceeded { .. } => CodingSessionError::Resource {
            message: format!("cannot capture rewind workspace snapshot: {error}"),
        },
    }
}

fn review_restore_error(error: WorkspaceRestoreError, operation_id: &str) -> CodingSessionError {
    if let WorkspaceRestoreError::Rollback { message } = error {
        CodingSessionError::PartialCommit {
            operation_id: operation_id.to_owned(),
            message: format!("workspace rewind rollback was incomplete: {message}"),
        }
    } else {
        CodingSessionError::Resource {
            message: format!("workspace rewind restore failed: {error}"),
        }
    }
}

fn workspace_restore_plan(
    current: &WorkspaceSnapshot,
    target: &WorkspaceSnapshot,
) -> WorkspaceRestorePlan {
    let mut paths = std::collections::BTreeSet::new();
    for file in current.files.iter().chain(&target.files) {
        paths.insert(file.path.clone());
    }
    let entries = paths
        .into_iter()
        .map(|path| WorkspaceRestoreEntry {
            expected: workspace_snapshot(current, &path),
            replacement: workspace_snapshot(target, &path),
        })
        .collect();
    WorkspaceRestorePlan { entries }
}

fn workspace_snapshot(snapshot: &WorkspaceSnapshot, path: &Path) -> WorkspaceFileSnapshot {
    match snapshot.file(path) {
        Some(file) => file.clone(),
        None => WorkspaceFileSnapshot {
            path: path.to_path_buf(),
            exists: false,
            revision: format!("{:x}", sha2::Sha256::digest([])),
            content: Some(Vec::new()),
        },
    }
}

fn validate_tracker_workspace(
    tracker: &HunkTrackerCheckpoint,
    workspace: &WorkspaceSnapshot,
) -> Result<(), CodingSessionError> {
    for file in &tracker.files {
        let Some(current) = &file.current else {
            continue;
        };
        let matches = match workspace.file(&file.path) {
            Some(snapshot) => current.exists && snapshot.revision == current.revision,
            None => !current.exists,
        };
        if !matches {
            return Err(CodingSessionError::Stale {
                message: format!(
                    "workspace changed while creating rewind checkpoint: {}",
                    file.path.display()
                ),
            });
        }
    }
    Ok(())
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
        let workspace = WorkspaceAccessHandle::open(
            WorkspaceHandle::new(WorkspaceKind::Projectless, dir.path()).unwrap(),
            None,
            None,
        )
        .unwrap();
        let service = ReviewService::new(workspace, snapshots.clone(), events);
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
