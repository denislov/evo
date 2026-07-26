use std::sync::Arc;

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{Context, StreamOptions};
use crate::providers;
use crate::registry::{
    ApiProvider, EnvProviderAuthResolver, ProviderAuthResolver, ProviderRegistry,
};

#[derive(Clone)]
pub struct AiClient {
    registry: ProviderRegistry,
    auth_resolver: Arc<dyn ProviderAuthResolver>,
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
        Self {
            registry: ProviderRegistry::new(),
            auth_resolver,
        }
    }

    pub fn with_registry(
        registry: ProviderRegistry,
        auth_resolver: Arc<dyn ProviderAuthResolver>,
    ) -> Self {
        Self {
            registry,
            auth_resolver,
        }
    }

    pub fn provider_registry(&self) -> ProviderRegistry {
        self.registry.clone()
    }

    pub fn register_provider(&self, api: impl Into<String>, provider: Arc<dyn ApiProvider>) {
        self.registry.register(api, provider);
    }

    pub fn register_builtins(&self) {
        providers::register_builtins_into(&self.registry);
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
}
