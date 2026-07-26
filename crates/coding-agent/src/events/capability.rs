use super::emission::ProductEventDraft;
use super::{
    CodingAgentCapabilityProductEvent, CodingAgentProductEventCapabilityRevocation,
    CodingAgentProductEventDurability, CodingAgentProductEventKind,
};
use crate::runtime::capability::CapabilityRevocationPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CapabilityEvent {
    Changed {
        generation: u64,
        revocation: CapabilityRevocationPolicy,
        cancellation_requested_operation_ids: Vec<String>,
    },
}

impl CapabilityEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Changed {
                generation,
                revocation,
                cancellation_requested_operation_ids,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Capability(
                    CodingAgentCapabilityProductEvent::Changed {
                        generation,
                        revocation: match revocation {
                            CapabilityRevocationPolicy::FutureOnly => {
                                CodingAgentProductEventCapabilityRevocation::FutureOnly
                            }
                            CapabilityRevocationPolicy::RequestCancelOlderOperations => {
                                CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations
                            }
                        },
                        cancellation_requested_operation_ids,
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
