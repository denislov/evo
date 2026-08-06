mod auth;
pub(crate) mod env;
mod provider;

pub(crate) use auth::options_contain_automatic_credentials;
pub use auth::{EnvProviderAuthResolver, ProviderAuth, ProviderAuthResolver};
pub use provider::{ApiProvider, ProviderRegistry};

#[cfg(test)]
mod provider_tests;
