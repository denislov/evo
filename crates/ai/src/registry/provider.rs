use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_stream::stream;

use super::auth::{
    EnvProviderAuthResolver, ProviderAuthResolver, apply_auth_material,
    options_contain_automatic_credentials, resolver_auth_will_apply,
    validate_automatic_credential_origin,
};
use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

pub trait ApiProvider: Send + Sync {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<HashMap<String, Arc<dyn ApiProvider>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, api: impl Into<String>, provider: Arc<dyn ApiProvider>) {
        self.providers.write().unwrap().insert(api.into(), provider);
    }

    pub fn unregister(&self, api: &str) {
        self.providers.write().unwrap().remove(api);
    }

    pub fn lookup(&self, api: &str) -> Option<Arc<dyn ApiProvider>> {
        self.providers.read().unwrap().get(api).cloned()
    }

    pub fn registered_apis(&self) -> Vec<String> {
        let mut apis = self
            .providers
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        apis.sort();
        apis
    }

    pub fn stream_model(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
    ) -> EventStream {
        self.stream_model_with_auth(model, ctx, opts, &EnvProviderAuthResolver)
    }

    pub fn stream_model_with_auth(
        &self,
        model: &Model,
        ctx: Context,
        mut opts: Option<StreamOptions>,
        auth_resolver: &dyn ProviderAuthResolver,
    ) -> EventStream {
        let api = model.api.clone();
        let provider = match self.lookup(&api) {
            Some(p) => p,
            None => return unknown_provider_stream(api),
        };

        let auth = auth_resolver.resolve_model_auth(model);
        let bind_resolver_auth = auth_resolver.requires_approved_https_origin()
            && resolver_auth_will_apply(opts.as_ref(), &auth);
        opts = apply_auth_material(opts, auth);
        let builtin_transport =
            crate::providers::builtin_provider_apis().contains(&model.api.as_str());
        if builtin_transport
            && (bind_resolver_auth || options_contain_automatic_credentials(opts.as_ref()))
            && let Err(message) = validate_automatic_credential_origin(model, opts.as_ref())
        {
            return credential_origin_error_stream(model, message);
        }

        provider.stream(model, ctx, opts)
    }
}

fn unknown_provider_stream(api: String) -> EventStream {
    Box::pin(stream! {
        let mut msg = AssistantMessage::empty("registry", "");
        msg.error_message = Some(format!("unknown provider api: {}", api));
        msg.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message: msg,
        };
    })
}

fn credential_origin_error_stream(model: &Model, message: String) -> EventStream {
    let api = model.api.clone();
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message_event = AssistantMessage::empty(&api, &model_id);
        message_event.provider = Some(provider);
        message_event.error_message = Some(message);
        message_event.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message: message_event,
        };
    })
}
