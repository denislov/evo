use std::sync::Arc;

use ai_protocol::api::conversation::Context;
use ai_protocol::api::model::Model;
use ai_protocol::api::stream::StreamOptions;

use super::HookFuture;
use crate::agent::types::ProviderRequestSnapshot;

pub type BeforeProviderRequestHook = Arc<
    dyn Fn(BeforeProviderRequestContext) -> HookFuture<Option<BeforeProviderRequestResult>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct BeforeProviderRequestContext {
    pub model: Model,
    pub context: Context,
    pub stream_options: StreamOptions,
}

impl From<ProviderRequestSnapshot> for BeforeProviderRequestContext {
    fn from(snapshot: ProviderRequestSnapshot) -> Self {
        Self {
            model: snapshot.model,
            context: snapshot.context,
            stream_options: snapshot.stream_options,
        }
    }
}

#[derive(Clone, Default)]
pub struct BeforeProviderRequestResult {
    pub context: Option<Context>,
    pub stream_options: Option<StreamOptions>,
}
