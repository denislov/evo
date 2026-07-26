mod agent;
mod provider;
mod tool;

use std::future::Future;
use std::pin::Pin;

pub type HookFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

pub use agent::{
    AgentHooks, AgentLoopTurnUpdate, ConvertToLlmHook, PrepareNextTurnContext, PrepareNextTurnHook,
    ShouldStopAfterTurnContext, ShouldStopAfterTurnHook, TransformContextHook,
};
pub use provider::{
    BeforeProviderRequestContext, BeforeProviderRequestHook, BeforeProviderRequestResult,
};
pub use tool::{
    AfterToolCallContext, AfterToolCallHook, AfterToolCallResult, BeforeToolCallContext,
    BeforeToolCallHook, BeforeToolCallResult,
};
