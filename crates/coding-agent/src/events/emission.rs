use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalStatus,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProductEventDraft {
    pub(crate) event: CodingAgentProductEventKind,
    pub(crate) operation_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) terminal_status: Option<CodingAgentProductEventTerminalStatus>,
    pub(crate) durability: CodingAgentProductEventDurability,
}

impl ProductEventDraft {
    pub(crate) fn with_durable_session(mut self, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        self.session_id = Some(session_id.clone());
        self.durability = CodingAgentProductEventDurability::Durable { session_id };
        self
    }
}
