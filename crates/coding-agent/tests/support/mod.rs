#![allow(deprecated)]

use std::ffi::{OsStr, OsString};
use std::sync::{Arc, Mutex, MutexGuard};

use ai::api::client::AiClient;
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::ApiProvider;
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[allow(
    dead_code,
    reason = "shared integration support is compiled into targets that do not assert public errors"
)]
pub fn assert_public_error(
    error: &CodingAgentPublicError,
    category: CodingAgentErrorCategory,
    code: &str,
    retryable: bool,
) {
    assert_eq!(error.category, category);
    assert_eq!(error.code(), code);
    assert_eq!(error.retryable, retryable);
    assert_eq!(error.context, CodingAgentErrorContext::None);
}

pub struct EnvGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

#[allow(
    dead_code,
    reason = "shared integration support is compiled separately by Cargo targets that use different environment helpers"
)]
impl EnvGuard<'static> {
    pub fn new(names: &[&'static str]) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        Self { _lock: lock, saved }
    }

    pub fn with_evo_dir<V: AsRef<OsStr>>(value: V) -> Self {
        let guard = Self::new(&["EVO_DIR"]);
        guard.set_evo_dir(value);
        guard
    }
}

#[allow(
    dead_code,
    reason = "shared integration support is compiled separately by Cargo targets that use different environment mutations"
)]
impl EnvGuard<'_> {
    pub fn set<V: AsRef<OsStr>>(&self, name: &str, value: V) {
        unsafe {
            std::env::set_var(name, value);
        }
    }

    pub fn remove(&self, name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
    }

    pub fn set_evo_dir<V: AsRef<OsStr>>(&self, value: V) {
        self.set("EVO_DIR", value);
    }
}

impl Drop for EnvGuard<'_> {
    fn drop(&mut self) {
        for (name, value) in self.saved.iter().rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

pub struct ProviderGuard {
    ai_client: AiClient,
}

#[allow(
    dead_code,
    reason = "shared integration support is compiled separately by Cargo targets that use different provider constructors"
)]
impl ProviderGuard {
    pub fn register(api: impl Into<String>, provider: Arc<dyn ApiProvider>) -> Self {
        Self::register_many(vec![(api.into(), provider)])
    }

    pub fn register_many(providers: Vec<(String, Arc<dyn ApiProvider>)>) -> Self {
        let ai_client = AiClient::new();
        for (api, provider) in providers {
            ai_client.register_provider(api, provider);
        }
        Self { ai_client }
    }

    pub fn ai_client(&self) -> AiClient {
        self.ai_client.clone()
    }
}

#[allow(
    dead_code,
    reason = "shared integration support is compiled by targets that do not all construct a primary model"
)]
pub fn model(api: &str) -> Model {
    named_model("test-model", "Test Model", api)
}

#[allow(
    dead_code,
    reason = "shared integration support is compiled by targets that do not all construct a fallback model"
)]
pub fn fallback_model(api: &str) -> Model {
    named_model("fallback-model", "Fallback Model", api)
}

fn named_model(id: &str, name: &str, api: &str) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: api.into(),
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}
