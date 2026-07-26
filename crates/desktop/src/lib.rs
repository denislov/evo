//! Native desktop adapter for the coding-agent product runtime.
//!
//! The crate is intentionally an adapter: product facts and mutable session
//! ownership remain in `coding-agent`.

use std::path::{Path, PathBuf};

extern crate self as desktop;

mod actions;
mod app;
mod command_ledger;
mod conversation;
mod file_review;
mod preferences;
mod projection;
mod runtime;
mod shell;

/// Supported startup inputs for the native desktop application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApplicationOptions {
    cwd: PathBuf,
    session_id: Option<String>,
}

impl DesktopApplicationOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            session_id: None,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

/// Run the native desktop application until its platform event loop exits.
pub fn run(options: DesktopApplicationOptions) {
    app::run(options);
}

#[cfg(test)]
mod tests {
    #[test]
    fn categorized_product_runtime_facade_is_importable() {
        let options = coding_agent::api::runtime::CodingAgentSessionOptions::new();
        assert!(options.cwd().is_none());
        let embedding = coding_agent::api::embedding::CodingAgentEmbeddingOptions::new(".");
        assert_eq!(embedding.cwd(), std::path::Path::new("."));
    }

    #[test]
    fn application_options_preserve_the_explicit_working_directory() {
        let options = super::DesktopApplicationOptions::new("project")
            .with_session_id("session-from-options");
        assert_eq!(options.cwd(), std::path::Path::new("project"));
        assert_eq!(options.session_id(), Some("session-from-options"));
    }
}
