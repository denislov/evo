pub mod auth;
pub mod paths;
pub mod settings;
mod storage;

use std::path::{Path, PathBuf};

pub use auth::AuthStore;
pub use paths::{ConfigPaths, resolve as resolve_paths};
pub use settings::{Settings, SettingsScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<PathBuf>,
}

impl ConfigDiagnostic {
    pub fn warn(message: impl Into<String>, source: Option<PathBuf>) -> Self {
        Self {
            severity: DiagnosticSeverity::Warn,
            message: message.into(),
            source,
        }
    }
}

pub struct Config {
    pub settings: Settings,
    pub auth: AuthStore,
}

pub fn load_config(cwd: &Path) -> (Config, Vec<ConfigDiagnostic>) {
    let mut diags = Vec::new();
    let paths = paths::resolve(cwd);
    let settings = settings::load_settings(&paths, &mut diags);
    let auth = AuthStore::load(&paths.global_auth(), &mut diags);
    (Config { settings, auth }, diags)
}
