//! `CodingAgentSession` facade methods for background tasks.
//!
//! Tasks are started through the `bash` tool (`background: true`); the tool
//! returns the task id in its structured details. These methods provide the
//! product-level query and control surface: list, output(cursor), wait
//! (single/any/all), cancel, and snapshot. All of them are thin projections
//! over the session's `BackgroundTaskService`.

use crate::services::background::{
    CodingAgentBackgroundTaskOutput, CodingAgentBackgroundTaskReport,
    CodingAgentBackgroundTaskSnapshot,
};

use super::CodingAgentSession;
use crate::kernel::error::CodingSessionError;
use crate::public_error::CodingAgentPublicError;
use workspace_runtime::api::{TaskId, TaskOwner};

fn parse_task_id(task_id: &str) -> Result<TaskId, CodingAgentPublicError> {
    let value = task_id.parse::<u64>().map_err(|_| {
        CodingAgentPublicError::from(CodingSessionError::Input {
            message: format!("invalid background task id: {task_id}"),
        })
    })?;
    Ok(TaskId::from_u64(value))
}

impl CodingAgentSession {
    pub fn background_task_list(&self) -> Vec<CodingAgentBackgroundTaskSnapshot> {
        self.runtime_host.background_tasks.list()
    }

    pub fn background_task_snapshot(
        &self,
        task_id: impl AsRef<str>,
    ) -> Result<CodingAgentBackgroundTaskSnapshot, CodingAgentPublicError> {
        let task_id = parse_task_id(task_id.as_ref())?;
        self.runtime_host
            .background_tasks
            .snapshot(task_id)
            .ok_or_else(|| {
                CodingAgentPublicError::from(CodingSessionError::Input {
                    message: format!("unknown background task: {task_id}"),
                })
            })
    }

    pub fn background_task_output(
        &self,
        task_id: impl AsRef<str>,
        cursor: u64,
    ) -> Result<CodingAgentBackgroundTaskOutput, CodingAgentPublicError> {
        let task_id = parse_task_id(task_id.as_ref())?;
        self.runtime_host
            .background_tasks
            .output(task_id, cursor)
            .ok_or_else(|| {
                CodingAgentPublicError::from(CodingSessionError::Input {
                    message: format!("unknown background task: {task_id}"),
                })
            })
    }

    /// Request termination of one task. Returns false when the task was
    /// already terminal or is unknown.
    pub fn background_task_cancel(&self, task_id: impl AsRef<str>) -> bool {
        let Ok(task_id) = parse_task_id(task_id.as_ref()) else {
            return false;
        };
        self.runtime_host.background_tasks.cancel(task_id)
    }

    /// Terminate every running task owned by `owner` (an `operation:` /
    /// `session:` / `worktree:` prefixed id as returned in task snapshots).
    /// Returns how many tasks were still running and therefore terminated.
    pub fn background_task_terminate_for_owner(&self, owner: &str) -> usize {
        let Some((kind, id)) = owner.split_once(':') else {
            return 0;
        };
        let task_owner = match kind {
            "operation" => TaskOwner::Operation(id.to_owned()),
            "session" => TaskOwner::Session(id.to_owned()),
            "worktree" => TaskOwner::Worktree(id.to_owned()),
            _ => return 0,
        };
        self.runtime_host
            .background_tasks
            .terminate_for_owner(&task_owner)
    }

    /// Resolve when the task leaves the `Running` state and return its final
    /// report (retained output, byte totals, and the explicit gap marker).
    pub async fn background_task_wait(
        &self,
        task_id: impl AsRef<str>,
    ) -> Result<CodingAgentBackgroundTaskReport, CodingAgentPublicError> {
        let task_id = parse_task_id(task_id.as_ref())?;
        Ok(self.runtime_host.background_tasks.wait(task_id).await)
    }

    /// Resolve when every listed task is terminal. Unknown ids report a
    /// `Failed` state with an empty output.
    pub async fn background_task_wait_all(
        &self,
        task_ids: &[String],
    ) -> Result<Vec<(String, CodingAgentBackgroundTaskReport)>, CodingAgentPublicError> {
        let mut ids = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            ids.push(parse_task_id(task_id)?);
        }
        Ok(self
            .runtime_host
            .background_tasks
            .wait_all(&ids)
            .await
            .into_iter()
            .map(|(id, report)| (id.to_string(), report))
            .collect())
    }

    /// Resolve when any listed task is terminal; returns its id and report.
    pub async fn background_task_wait_any(
        &self,
        task_ids: &[String],
    ) -> Result<Option<(String, CodingAgentBackgroundTaskReport)>, CodingAgentPublicError> {
        let mut ids = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            ids.push(parse_task_id(task_id)?);
        }
        Ok(self
            .runtime_host
            .background_tasks
            .wait_any(&ids)
            .await
            .map(|(id, report)| (id.to_string(), report)))
    }
}
