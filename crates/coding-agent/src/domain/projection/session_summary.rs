use crate::session::repository::SessionSummary;
use crate::session::view::CodingAgentSessionSummary;
use crate::session::view::SessionStorageHandle;

impl From<SessionSummary> for CodingAgentSessionSummary {
    fn from(summary: SessionSummary) -> Self {
        let storage = SessionStorageHandle::new(
            summary.session_id.clone(),
            summary.session_dir,
            summary.event_log_name,
        );
        Self {
            session_id: summary.session_id,
            name: summary.name,
            storage,
            created_at: summary.created_at,
            updated_at: summary.updated_at,
            active_leaf_id: summary.active_leaf_id,
        }
    }
}
