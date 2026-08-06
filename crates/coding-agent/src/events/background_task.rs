use super::emission::ProductEventDraft;
use super::{
    CodingAgentBackgroundTaskProductEvent, CodingAgentProductEventDurability,
    CodingAgentProductEventKind, CodingAgentProductEventTerminalStatus,
};

/// Live-only background task lifecycle events, emitted by the
/// `BackgroundTaskService` as tasks start and finish. Output contents are
/// fetched through the cursor API, never embedded in events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackgroundTaskEvent {
    Started {
        task_id: String,
        owner: String,
    },
    Completed {
        task_id: String,
        exit_code: Option<i32>,
        dropped_bytes: Option<u64>,
    },
    Cancelled {
        task_id: String,
    },
    TimedOut {
        task_id: String,
    },
    Failed {
        task_id: String,
        message: String,
    },
}

impl BackgroundTaskEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, terminal_status) = match self {
            Self::Started { task_id, owner } => (
                CodingAgentBackgroundTaskProductEvent::Started { task_id, owner },
                None,
            ),
            Self::Completed {
                task_id,
                exit_code,
                dropped_bytes,
            } => (
                CodingAgentBackgroundTaskProductEvent::Completed {
                    task_id,
                    exit_code,
                    dropped_bytes,
                },
                Some(CodingAgentProductEventTerminalStatus::Completed),
            ),
            Self::Cancelled { task_id } => (
                CodingAgentBackgroundTaskProductEvent::Cancelled { task_id },
                Some(CodingAgentProductEventTerminalStatus::Aborted),
            ),
            Self::TimedOut { task_id } => (
                CodingAgentBackgroundTaskProductEvent::TimedOut { task_id },
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
            Self::Failed { task_id, message } => (
                CodingAgentBackgroundTaskProductEvent::Failed { task_id, message },
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::BackgroundTask(event),
            operation_id: None,
            session_id: None,
            terminal_status,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}
