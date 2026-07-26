mod auth;
pub(crate) mod env;
mod provider;

pub use auth::{EnvProviderAuthResolver, ProviderAuth, ProviderAuthResolver};
pub use provider::{ApiProvider, ProviderRegistry};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AiClient;
    use crate::model::{Model, ModelCost, ModelInput};
    use crate::protocol::stream::EventStream;
    use crate::protocol::{
        AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
    };
    use async_stream::stream;
    use futures::StreamExt;
    use std::sync::Arc;

    struct DummyProvider;
    impl ApiProvider for DummyProvider {
        fn stream(
            &self,
            _model: &Model,
            _ctx: Context,
            _opts: Option<StreamOptions>,
        ) -> EventStream {
            Box::pin(stream! {
                let mut msg = AssistantMessage::empty("dummy", "dummy");
                msg.content.push(crate::protocol::ContentBlock::Text {
                    text: "dummy response".into(), text_signature: None,
                });
                yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg };
            })
        }
    }

    #[tokio::test]
    async fn scoped_registry_register_and_lookup() {
        let client = AiClient::new();
        client.register_provider("reg-test-api", Arc::new(DummyProvider));
        let found = client.lookup_provider("reg-test-api");
        assert!(found.is_some());
        client.unregister_provider("reg-test-api");
        assert!(client.lookup_provider("reg-test-api").is_none());
    }

    #[tokio::test]
    async fn stream_model_unknown_api_returns_error() {
        let model = Model {
            id: "x".into(),
            name: "x".into(),
            api: "nonexistent".into(),
            provider: "none".into(),
            base_url: "".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        };
        let mut stream = AiClient::new().stream_model(
            &model,
            Context {
                system_prompt: None,
                messages: vec![],
                tools: None,
            },
            None,
        );
        let event = stream.next().await.unwrap();
        assert!(matches!(event, AssistantMessageEvent::Error { .. }));
    }

    #[tokio::test]
    async fn stream_model_delegates_to_provider() {
        let client = AiClient::new();
        client.register_provider("test-api", Arc::new(DummyProvider));
        let model = Model {
            id: "x".into(),
            name: "x".into(),
            api: "test-api".into(),
            provider: "test".into(),
            base_url: "".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                known: true,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        };
        let mut stream = client.stream_model(
            &model,
            Context {
                system_prompt: None,
                messages: vec![],
                tools: None,
            },
            None,
        );
        let event = stream.next().await.unwrap();
        assert!(matches!(event, AssistantMessageEvent::Done { .. }));
    }
}
