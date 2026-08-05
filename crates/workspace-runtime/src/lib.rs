//! Typed workspace contracts and bounded filesystem/process capabilities.
//!
//! This crate deliberately owns no product session, authorization UI, or AI
//! conversation types. Capabilities are leased by a workspace owner and stay
//! in the platform layer, so a child workspace can be handed between adapters
//! without passing ambient authority.

mod access;
mod contract;
mod error;
mod resource;

mod fs;
mod process;
mod worktree;

/// Stable workspace ownership, lifecycle, and capability facade.
pub mod api;
