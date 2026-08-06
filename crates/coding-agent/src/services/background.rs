//! Product-level background task service: the coding-agent surface over the
//! workspace-runtime `TaskRegistry`.
//!
//! Background tasks are spawned by the `bash` tool (`background: true`),
//! survive the tool call, and are queried and controlled through this service
//! (list / output cursor / wait / cancel / snapshot). The service owns the
//! session-close policy: `shutdown` terminates every task and joins its
//! driver, and `terminate_for_owner` applies the same termination to one
//! owner group. Task lifecycle events (started and each terminal state) are
//! emitted as live product events; output bytes travel through the cursor
//! API, never inside events.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::events::background_task::BackgroundTaskEvent;
use crate::services::event::EventService;

use workspace_runtime::api::{ProcessSpec, TaskId, TaskOwner, TaskRegistry, TaskSpawnError};

/// Public projection of one background task, without output contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentBackgroundTaskSnapshot {
    pub task_id: String,
    pub owner: String,
    pub spawned_at_ms: u64,
    pub state: CodingAgentBackgroundTaskState,
    pub total_bytes: u64,
    pub dropped_bytes: Option<u64>,
}

/// Public projection of the task state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentBackgroundTaskState {
    Running,
    Completed { exit_code: Option<i32> },
    TimedOut,
    Cancelled,
    Failed,
}

/// Public incremental output read: text since `cursor`, the cursor to pass
/// next, and an explicit gap marker when the spool dropped bytes the reader
/// missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentBackgroundTaskOutput {
    pub text: String,
    pub next_cursor: u64,
    pub dropped_bytes: Option<u64>,
}

/// Public final report of a task that left the `Running` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentBackgroundTaskReport {
    pub state: CodingAgentBackgroundTaskState,
    pub output: String,
    pub total_bytes: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub dropped_bytes: Option<u64>,
}

/// Lightweight product service for background tasks. One instance lives in
/// each `RuntimeHost`; it is `Clone` so tools and facade methods share the
/// same registry.
#[derive(Clone)]
pub(crate) struct BackgroundTaskService {
    registry: Arc<TaskRegistry>,
    events: EventService,
}

impl std::fmt::Debug for BackgroundTaskService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundTaskService")
            .field("tasks", &self.registry.list().len())
            .finish()
    }
}

impl BackgroundTaskService {
    pub(crate) fn new(events: EventService) -> Self {
        Self {
            registry: Arc::new(TaskRegistry::new()),
            events,
        }
    }

    /// Spawn a background task under `owner` and watch it for lifecycle
    /// events. `timeout` is the task budget: `None` means no hard deadline
    /// (bounded only by cancellation, owner termination, and session close).
    pub(crate) async fn start(
        &self,
        spec: ProcessSpec,
        owner: TaskOwner,
        timeout: Option<Duration>,
    ) -> Result<TaskId, TaskSpawnError> {
        let handle = self.registry.spawn(spec, owner.clone(), timeout).await?;
        let task_id = handle.task_id();
        let _ = self
            .events
            .emit_background_task(BackgroundTaskEvent::Started {
                task_id: task_id.to_string(),
                owner: owner.to_string(),
            });
        let events = self.events.clone();
        let watcher = handle.clone();
        tokio::spawn(async move {
            let report = watcher.wait().await;
            let event = match report.state {
                workspace_runtime::api::TaskState::Completed { exit_code } => {
                    BackgroundTaskEvent::Completed {
                        task_id: task_id.to_string(),
                        exit_code,
                        dropped_bytes: report.gap.map(|gap| gap.dropped_bytes),
                    }
                }
                workspace_runtime::api::TaskState::Cancelled => BackgroundTaskEvent::Cancelled {
                    task_id: task_id.to_string(),
                },
                workspace_runtime::api::TaskState::TimedOut => BackgroundTaskEvent::TimedOut {
                    task_id: task_id.to_string(),
                },
                workspace_runtime::api::TaskState::Failed { message } => {
                    BackgroundTaskEvent::Failed {
                        task_id: task_id.to_string(),
                        message,
                    }
                }
                workspace_runtime::api::TaskState::Running => {
                    unreachable!("watcher resolves only after the task leaves the running state")
                }
            };
            let _ = events.emit_background_task(event);
        });
        Ok(task_id)
    }

    pub(crate) fn list(&self) -> Vec<CodingAgentBackgroundTaskSnapshot> {
        self.registry.list().into_iter().map(snapshot_dto).collect()
    }

    pub(crate) fn output(
        &self,
        task_id: TaskId,
        cursor: u64,
    ) -> Option<CodingAgentBackgroundTaskOutput> {
        self.registry.task(task_id).map(|handle| {
            let chunk = handle.output(cursor);
            CodingAgentBackgroundTaskOutput {
                text: chunk.text,
                next_cursor: chunk.next_cursor,
                dropped_bytes: chunk.gap.map(|gap| gap.dropped_bytes),
            }
        })
    }

    pub(crate) fn snapshot(&self, task_id: TaskId) -> Option<CodingAgentBackgroundTaskSnapshot> {
        self.registry
            .task(task_id)
            .map(|handle| snapshot_dto(handle.snapshot()))
    }

    pub(crate) fn cancel(&self, task_id: TaskId) -> bool {
        self.registry.cancel(task_id)
    }

    pub(crate) fn terminate_for_owner(&self, owner: &TaskOwner) -> usize {
        self.registry.terminate_all_for_owner(owner)
    }

    pub(crate) async fn wait(&self, task_id: TaskId) -> CodingAgentBackgroundTaskReport {
        let report = match self.registry.task(task_id) {
            Some(handle) => handle.wait().await,
            None => {
                return unknown_task_report();
            }
        };
        report_dto(report)
    }

    pub(crate) async fn wait_all(
        &self,
        task_ids: &[TaskId],
    ) -> Vec<(TaskId, CodingAgentBackgroundTaskReport)> {
        self.registry
            .wait_all(task_ids)
            .await
            .into_iter()
            .map(|(task_id, report)| (task_id, report_dto(report)))
            .collect()
    }

    pub(crate) async fn wait_any(
        &self,
        task_ids: &[TaskId],
    ) -> Option<(TaskId, CodingAgentBackgroundTaskReport)> {
        self.registry
            .wait_any(task_ids)
            .await
            .map(|(task_id, report)| (task_id, report_dto(report)))
    }

    /// Session-close policy: terminate every running task and join its
    /// driver. Returns the number of tasks that were still running.
    pub(crate) async fn shutdown(&self) -> usize {
        self.registry.shutdown().await
    }
}

fn snapshot_dto(
    snapshot: workspace_runtime::api::TaskSnapshot,
) -> CodingAgentBackgroundTaskSnapshot {
    CodingAgentBackgroundTaskSnapshot {
        task_id: snapshot.task_id.to_string(),
        owner: snapshot.owner.to_string(),
        spawned_at_ms: spawned_at_millis(snapshot.spawned_at),
        state: state_dto(snapshot.state),
        total_bytes: snapshot.total_bytes,
        dropped_bytes: snapshot.gap.map(|gap| gap.dropped_bytes),
    }
}

fn spawned_at_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn state_dto(state: workspace_runtime::api::TaskState) -> CodingAgentBackgroundTaskState {
    match state {
        workspace_runtime::api::TaskState::Running => CodingAgentBackgroundTaskState::Running,
        workspace_runtime::api::TaskState::Completed { exit_code } => {
            CodingAgentBackgroundTaskState::Completed { exit_code }
        }
        workspace_runtime::api::TaskState::TimedOut => CodingAgentBackgroundTaskState::TimedOut,
        workspace_runtime::api::TaskState::Cancelled => CodingAgentBackgroundTaskState::Cancelled,
        workspace_runtime::api::TaskState::Failed { .. } => CodingAgentBackgroundTaskState::Failed,
    }
}

fn report_dto(report: workspace_runtime::api::TaskReport) -> CodingAgentBackgroundTaskReport {
    CodingAgentBackgroundTaskReport {
        state: state_dto(report.state),
        output: report.output,
        total_bytes: report.total_bytes,
        stdout_bytes: report.stdout_bytes,
        stderr_bytes: report.stderr_bytes,
        dropped_bytes: report.gap.map(|gap| gap.dropped_bytes),
    }
}

fn unknown_task_report() -> CodingAgentBackgroundTaskReport {
    CodingAgentBackgroundTaskReport {
        state: CodingAgentBackgroundTaskState::Failed,
        output: String::new(),
        total_bytes: 0,
        stdout_bytes: 0,
        stderr_bytes: 0,
        dropped_bytes: None,
    }
}

#[cfg(test)]
mod tests_background_service;
