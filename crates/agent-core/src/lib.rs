mod agent;
mod compaction;
mod context;
mod execution;
mod hooks;
mod resources;
mod transcript;

/// Stable low-level runtime facade for `agent-core`.
///
/// Product session ownership, adapter wire events, and workflow ownership belong
/// in `coding-agent`. This module intentionally exposes low-level agent,
/// tool, hook, resource, and environment contracts.
pub mod api;
