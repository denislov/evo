pub mod app;
mod clipboard;
mod commands;
mod delegation_confirmation_menu;
mod error;
pub mod event_bridge;
#[cfg(test)]
mod event_bridge_tests;
mod git_branch;
mod input;
pub mod key_hints;
pub(crate) mod keybindings;
mod r#loop;
mod model_selector;
mod profile_menu;
mod prompt_task;
mod render;
mod root;
mod session_actions;
mod session_selector;
mod slash;
pub(crate) mod syntax;
pub(crate) mod theme;
pub mod transcript;
#[cfg(test)]
mod transcript_tests;
mod transient_overlay;
pub(super) mod tree_selector;

pub use app::run_interactive_mode;
pub use event_bridge::UiEvent;
pub use transcript::{Transcript, TranscriptItem};
