use crate::support;

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
    let api = "support-provider-guard-scoped-api";
    let guard = support::ProviderGuard::register(api, Arc::new(GuardTestProvider));
    let unrelated = support::ProviderGuard::register(
        "support-provider-guard-unrelated-api",
        Arc::new(GuardTestProvider),
    );

    assert!(guard.ai_client().lookup_provider(api).is_some());
    assert!(unrelated.ai_client().lookup_provider(api).is_none());
}

#[test]
fn provider_guard_register_many_populates_one_scoped_client() {
    let first_api = "support-provider-guard-many-first";
    let second_api = "support-provider-guard-many-second";
    let providers: Vec<(String, Arc<dyn ApiProvider>)> = vec![
        (first_api.to_string(), Arc::new(GuardTestProvider)),
        (second_api.to_string(), Arc::new(GuardTestProvider)),
    ];
    let guard = support::ProviderGuard::register_many(providers);
    let client = guard.ai_client();

    assert!(client.lookup_provider(first_api).is_some());
    assert!(client.lookup_provider(second_api).is_some());
}
