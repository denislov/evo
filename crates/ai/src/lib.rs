mod client;
mod compatibility;
mod model;
// Provider wire fields are intentionally deserialized even when the generic
// runtime does not read every field, and several provider helpers are exercised
// only by owner tests. Keep the allowance scoped to this private implementation
// tree; it is not part of the public facade contract.
mod protocol;
mod providers;
mod registry;
#[cfg(test)]
mod regression_tests;
mod transport;

/// Stable facade for embedding `ai`.
///
/// Implementation owners are private. Provider registration and streaming are
/// scoped to `AiClient` or `ProviderRegistry`; downstream code imports only a
/// categorized path under this module.
pub mod api;
