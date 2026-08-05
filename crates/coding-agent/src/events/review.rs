use serde::{Deserialize, Serialize};

use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind, CodingAgentReviewProductEvent,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentReviewChange {
    pub path: String,
    pub mutation_kind: String,
    pub source: String,
    pub operation_id: String,
    pub tool_call_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub updated_sequence: u64,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub after_exists: bool,
    pub first_changed_line: Option<usize>,
    pub added_lines: Option<usize>,
    pub removed_lines: Option<usize>,
    pub diff: Option<String>,
    pub hunks: Vec<CodingAgentReviewHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentReviewHunk {
    pub id: String,
    pub source: String,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub diff: Option<String>,
}

pub(crate) fn changed_draft(changes: Vec<CodingAgentReviewChange>) -> ProductEventDraft {
    ProductEventDraft {
        event: CodingAgentProductEventKind::Review(CodingAgentReviewProductEvent::Changed {
            changes,
        }),
        operation_id: None,
        session_id: None,
        terminal_status: None,
        durability: CodingAgentProductEventDurability::LiveOnly,
    }
}
