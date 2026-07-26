use crate::model::Model;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ProviderPayloadHookFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>;
pub type ProviderResponseHookFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
pub type ProviderPayloadHook =
    Arc<dyn Fn(Model, serde_json::Value) -> ProviderPayloadHookFuture + Send + Sync>;
pub type ProviderResponseHook =
    Arc<dyn Fn(ProviderResponseInfo) -> ProviderResponseHookFuture + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderResponseInfo {
    pub status: Option<u16>,
}

#[derive(Clone, Default)]
pub struct ProviderStreamHooks {
    pub on_payload: Option<ProviderPayloadHook>,
    pub on_response: Option<ProviderResponseHook>,
}

impl std::fmt::Debug for ProviderStreamHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderStreamHooks")
            .field("on_payload", &self.on_payload.is_some())
            .field("on_response", &self.on_response.is_some())
            .finish()
    }
}

impl ProviderStreamHooks {
    pub async fn apply_payload(
        &self,
        model: &Model,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match self.on_payload.as_ref() {
            Some(hook) => hook(model.clone(), payload).await,
            None => Ok(payload),
        }
    }

    pub async fn emit_response(&self, response: ProviderResponseInfo) -> Result<(), String> {
        match self.on_response.as_ref() {
            Some(hook) => hook(response).await,
            None => Ok(()),
        }
    }
}
