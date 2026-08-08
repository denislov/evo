use std::sync::Arc;

use crate::model::Model;
use crate::protocol::stream::{EventStream, complete};
use crate::protocol::{AssistantMessage, Context, StreamOptions};
use crate::providers;
use crate::registry::{
    ApiProvider, EnvProviderAuthResolver, ProviderAuthResolver, ProviderRegistry,
};
use crate::transport::circuit_breaker::{BreakerKey, CircuitBreakerConfig, CircuitBreakerRegistry};
use crate::transport::http::SendResilience;
use observability::SecretStore;

#[derive(Clone)]
pub struct AiClient {
    registry: ProviderRegistry,
    auth_resolver: Arc<dyn ProviderAuthResolver>,
    transport_client: reqwest::Client,
    breaker_registry: Arc<CircuitBreakerRegistry>,
    secrets: Arc<SecretStore>,
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
            breaker_registry: Arc::new(
                CircuitBreakerRegistry::new(CircuitBreakerConfig::default()),
            ),
            secrets: Arc::new(SecretStore::default()),
        })
    }

    pub fn with_registry(
        registry: ProviderRegistry,
        auth_resolver: Arc<dyn ProviderAuthResolver>,
    ) -> Self {
        let mut client = Self::try_with_auth_resolver_and_transport(
            auth_resolver,
            crate::transport::client::TransportConfig::default(),
        )
        .expect("the default rustls HTTP client configuration should build");
        client.registry = registry;
        client
    }

    /// Replace the shared circuit breaker registry. All subsequent
    /// invocations isolate failure state per `(provider, api)` key.
    pub fn with_breaker_registry(mut self, registry: Arc<CircuitBreakerRegistry>) -> Self {
        self.breaker_registry = registry;
        self
    }

    pub fn breaker_registry(&self) -> Arc<CircuitBreakerRegistry> {
        self.breaker_registry.clone()
    }

    /// Register a credential value so outgoing error messages are redacted.
    /// Automatic credentials are remembered on every invocation; explicit
    /// callers can register additional values.
    pub fn remember_secret(&self, secret: impl Into<String>) {
        self.secrets.remember(secret);
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
        if let Some(api_key) = opts.as_ref().and_then(|o| o.api_key.as_deref()) {
            self.secrets.remember(api_key);
        }
        let breaker = self
            .breaker_registry
            .breaker_for(BreakerKey::new(&model.provider, &model.api));
        let scrubber = Arc::new(self.secrets.snapshot());
        self.registry.stream_model_with_resilience(
            model,
            ctx,
            opts,
            self.auth_resolver.clone(),
            SendResilience {
                breaker: Some(breaker),
                scrubber: Some(scrubber),
                refresh_auth: None,
            },
        )
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

    #[test]
    fn invalid_extra_ca_pem_fails_construction() {
        let transport = TransportConfig::new(None, None).with_extra_ca(vec![
            b"-----BEGIN CERTIFICATE-----\nnot-valid-base64-content\n-----END CERTIFICATE-----"
                .to_vec(),
        ]);
        let error = AiClient::try_with_auth_resolver_and_transport(
            Arc::new(EnvProviderAuthResolver),
            transport,
        )
        .err()
        .expect("invalid PEM fails before registration");
        assert!(
            error.contains("invalid extra CA certificate PEM")
                || error.contains("failed to build provider HTTP client"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn valid_extra_ca_builds_a_client() {
        let transport = TransportConfig::new(None, None).with_extra_ca(vec![
            include_bytes!("transport/fixtures/test-ca.pem").to_vec(),
        ]);
        let client = AiClient::try_with_auth_resolver_and_transport(
            Arc::new(EnvProviderAuthResolver),
            transport,
        )
        .expect("valid CA PEM builds a client");
        client.register_builtins();
        assert!(client.lookup_provider("openai-responses").is_some());
    }

    #[test]
    fn empty_ca_bundle_is_normalized_away() {
        let transport = TransportConfig::new(None, None).with_extra_ca(vec![]);
        assert!(transport.extra_ca_certificates.is_none());
    }
}
