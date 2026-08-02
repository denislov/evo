use std::sync::Arc;

use crate::model::Model;
use crate::protocol::stream::{EventStream, complete};
use crate::protocol::{AssistantMessage, Context, StreamOptions};
use crate::providers;
use crate::registry::{
    ApiProvider, EnvProviderAuthResolver, ProviderAuthResolver, ProviderRegistry,
};

#[derive(Clone)]
pub struct AiClient {
    registry: ProviderRegistry,
    auth_resolver: Arc<dyn ProviderAuthResolver>,
    transport_client: reqwest::Client,
}

impl Default for AiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AiClient {
    pub fn new() -> Self {
        Self::with_auth_resolver(Arc::new(EnvProviderAuthResolver))
    }

    pub fn with_auth_resolver(auth_resolver: Arc<dyn ProviderAuthResolver>) -> Self {
        Self::try_with_auth_resolver_and_transport(
            auth_resolver,
            crate::transport::client::TransportConfig::default(),
        )
        .expect("the default rustls HTTP client configuration should build")
    }

    pub fn try_with_auth_resolver_and_transport(
        auth_resolver: Arc<dyn ProviderAuthResolver>,
        transport: crate::transport::client::TransportConfig,
    ) -> Result<Self, String> {
        Ok(Self {
            registry: ProviderRegistry::new(),
            auth_resolver,
            transport_client: crate::transport::client::authenticated_client(&transport)?,
        })
    }

    pub fn with_registry(
        registry: ProviderRegistry,
        auth_resolver: Arc<dyn ProviderAuthResolver>,
    ) -> Self {
        Self {
            registry,
            auth_resolver,
            transport_client: crate::transport::client::authenticated_client(
                &crate::transport::client::TransportConfig::default(),
            )
            .expect("the default rustls HTTP client configuration should build"),
        }
    }

    pub fn provider_registry(&self) -> ProviderRegistry {
        self.registry.clone()
    }

    pub fn register_provider(&self, api: impl Into<String>, provider: Arc<dyn ApiProvider>) {
        self.registry.register(api, provider);
    }

    pub fn register_builtins(&self) {
        providers::register_builtins_into(&self.registry, &self.transport_client);
    }

    pub fn unregister_provider(&self, api: &str) {
        self.registry.unregister(api);
    }

    pub fn lookup_provider(&self, api: &str) -> Option<Arc<dyn ApiProvider>> {
        self.registry.lookup(api)
    }

    pub fn stream_model(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
    ) -> EventStream {
        self.registry
            .stream_model_with_auth(model, ctx, opts, self.auth_resolver.as_ref())
    }

    /// Execute a model request and collect its stream into one terminal message.
    ///
    /// Providers retain one streaming implementation and this convenience API
    /// gives non-incremental consumers a complete response without duplicating
    /// provider transports or parsing state machines.
    pub async fn complete_model(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
    ) -> Result<AssistantMessage, String> {
        complete(self.stream_model(model, ctx, opts)).await
    }
}

#[cfg(test)]
mod tests {
    use super::AiClient;
    use crate::protocol::{ContentBlock, Context};
    use crate::providers::faux::FauxProvider;
    use crate::registry::EnvProviderAuthResolver;
    use crate::transport::client::TransportConfig;
    use std::sync::Arc;

    #[tokio::test]
    async fn complete_model_collects_the_registered_provider_stream() {
        let client = AiClient::new();
        client.register_provider("test-faux", Arc::new(FauxProvider::simple_text("complete")));
        let mut model =
            crate::model::get_model("deepseek", "deepseek-v4-flash").expect("catalog model exists");
        model.api = "test-faux".into();

        let message = client
            .complete_model(
                &model,
                Context {
                    system_prompt: None,
                    messages: Vec::new(),
                    tools: None,
                },
                None,
            )
            .await
            .expect("stream completes");
        assert!(matches!(
            message.content.as_slice(),
            [ContentBlock::Text { text, .. }] if text == "complete"
        ));
    }

    #[test]
    fn configured_proxy_and_connect_timeout_build_a_scoped_transport() {
        let client = AiClient::try_with_auth_resolver_and_transport(
            Arc::new(EnvProviderAuthResolver),
            TransportConfig::new(Some("http://127.0.0.1:8888".into()), Some(4_321)),
        )
        .expect("valid proxy and timeout build a client");
        client.register_builtins();
        assert!(client.lookup_provider("openai-responses").is_some());
    }

    #[test]
    fn invalid_transport_settings_fail_before_provider_registration() {
        assert!(
            AiClient::try_with_auth_resolver_and_transport(
                Arc::new(EnvProviderAuthResolver),
                TransportConfig::new(Some("://bad proxy".into()), Some(1_000)),
            )
            .is_err()
        );
        assert!(
            AiClient::try_with_auth_resolver_and_transport(
                Arc::new(EnvProviderAuthResolver),
                TransportConfig::new(None, Some(0)),
            )
            .is_err()
        );
    }
}
