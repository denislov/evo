use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::api::client::AiClient;
use crate::api::provider::ApiProvider;

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct EnvGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

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
}

impl EnvGuard<'_> {
    pub fn set(&self, name: &str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }

    pub fn remove(&self, name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
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

impl ProviderGuard {
    pub fn for_api(api: impl Into<String>) -> Self {
        let _ = api.into();
        Self {
            ai_client: AiClient::new(),
        }
    }

    pub fn clear(api: impl Into<String>) -> Self {
        Self::for_api(api)
    }

    pub fn register(api: impl Into<String>, provider: Arc<dyn ApiProvider>) -> Self {
        let api = api.into();
        let ai_client = AiClient::new();
        ai_client.register_provider(api, provider);
        Self { ai_client }
    }

    pub fn ai_client(&self) -> AiClient {
        self.ai_client.clone()
    }
}
