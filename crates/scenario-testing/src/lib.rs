//! Shared deterministic scenario contracts for product adapter tests.
//!
//! The crate intentionally owns test inputs and semantic oracles, not UI
//! renderers. CLI and Desktop remain free to present the same product state in
//! different ways.

mod contract;
mod loader;
mod product;
mod sse;
mod terminal;

pub use contract::*;
pub use loader::*;
pub use product::*;
pub use sse::*;
pub use terminal::*;
