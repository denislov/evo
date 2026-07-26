use crate::common;

use std::sync::Arc;

use ai::api::conversation::{AssistantMessage, Context, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use async_stream::stream;

struct GuardTestProvider;

impl ApiProvider for GuardTestProvider {
    fn stream(&self, _model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        Box::pin(stream! {
            let message = AssistantMessage::empty("guard", "guard response");
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

#[test]
fn provider_guard_registers_only_its_scoped_client() {
    let api = "agent-core-provider-guard-drop-api";
    let guard = common::ProviderGuard::register(api, Arc::new(GuardTestProvider));
    assert!(guard.ai_client().lookup_provider(api).is_some());
    assert!(
        ai::api::client::AiClient::new()
            .lookup_provider(api)
            .is_none()
    );
}
