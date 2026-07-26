use super::support;

use std::sync::Arc;

use ai::api::provider::ApiProvider;
use ai::model::Model;
use ai::protocol::stream::EventStream;
use ai::protocol::{AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions};
use async_stream::stream;

struct GuardTestProvider(&'static str);

impl ApiProvider for GuardTestProvider {
    fn stream(&self, _model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let text = self.0.to_string();
        Box::pin(stream! {
            let mut message = AssistantMessage::empty("guard", "guard-model");
            message.content.push(ai::protocol::ContentBlock::Text {
                text,
                text_signature: None,
            });
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

#[test]
fn provider_guard_registers_only_its_scoped_client() {
    let api = "ai-provider-guard-drop-api";
    let guard = support::ProviderGuard::register(api, Arc::new(GuardTestProvider("temp")));
    assert!(guard.ai_client().lookup_provider(api).is_some());
    assert!(
        ai::api::client::AiClient::new()
            .lookup_provider(api)
            .is_none()
    );
}

#[test]
fn provider_guard_clear_starts_with_an_empty_scoped_client() {
    let api = "ai-provider-guard-clear-api";
    let guard = support::ProviderGuard::clear(api);
    assert!(guard.ai_client().lookup_provider(api).is_none());
}

#[test]
fn provider_guard_instances_are_isolated() {
    let api = "ai-provider-guard-restore-api";
    let first = support::ProviderGuard::register(api, Arc::new(GuardTestProvider("first")));
    let second = support::ProviderGuard::register(api, Arc::new(GuardTestProvider("second")));
    assert!(first.ai_client().lookup_provider(api).is_some());
    assert!(second.ai_client().lookup_provider(api).is_some());
}
// Internal support-contract tests.
