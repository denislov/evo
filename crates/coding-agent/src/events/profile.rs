use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind, CodingAgentProfileProductEvent,
};
use crate::profiles::ProfileId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileEvent {
    DefaultChanged { profile_id: ProfileId },
}

impl ProfileEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::DefaultChanged { profile_id } => ProductEventDraft {
                event: CodingAgentProductEventKind::Profile(
                    CodingAgentProfileProductEvent::DefaultChanged {
                        profile_id: profile_id.as_str().to_owned(),
                    },
                ),
                operation_id: None,
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
        }
    }
}
