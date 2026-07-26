use crate::protocol::events::CodingProtocolEventAdapter;
use crate::protocol::types::ProtocolEvent;
use coding_agent::api::event::CodingAgentProductEvent;

pub(crate) struct RpcCodingEventAdapter {
    inner: CodingProtocolEventAdapter,
}

impl RpcCodingEventAdapter {
    pub(crate) fn new_with_provider(api: String, provider: String, model: String) -> Self {
        Self {
            inner: CodingProtocolEventAdapter::new_with_provider(api, provider, model),
        }
    }

    pub(crate) fn push_product_event(
        &mut self,
        event: &CodingAgentProductEvent,
    ) -> Vec<ProtocolEvent> {
        self.inner.push_product_event(event)
    }
}
