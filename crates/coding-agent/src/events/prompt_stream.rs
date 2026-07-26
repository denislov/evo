use super::agent::AgentStreamEvent;
use super::delegation::DelegationEvent;
use super::emission::ProductEventDraft;
use super::message::MessageEvent;
use super::runtime::RuntimeEvent;
use super::tool::ToolEvent;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PromptStreamEvent {
    Agent(AgentStreamEvent),
    Message(MessageEvent),
    Tool(ToolEvent),
    Delegation(DelegationEvent),
    Runtime(RuntimeEvent),
}

impl PromptStreamEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Agent(event) => event.into_product_draft(),
            Self::Message(event) => event.into_product_draft(),
            Self::Tool(event) => event.into_product_draft(),
            Self::Delegation(event) => event.into_product_draft(),
            Self::Runtime(event) => event.into_product_draft(),
        }
    }
}
