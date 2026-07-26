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

/// Render diagnostics for stderr. Empty string when there are none.
#[cfg(test)]
pub fn drain_diagnostics(diags: &[ConfigDiagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        let label = match d.severity {
            DiagnosticSeverity::Warn => "warning",
        };
        match &d.source {
            Some(p) => out.push_str(&format!(
                "config {label}: {} ({})\n",
                d.message,
                p.display()
            )),
            None => out.push_str(&format!("config {label}: {}\n", d.message)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config_reads_settings_and_auth_from_evo_dir() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("settings.toml"), "default_model = \"m\"\n").unwrap();
        std::fs::write(
            global.join("auth.toml"),
            "[anthropic]\ntype=\"api_key\"\nkey=\"sk-x\"\n",
        )
        .unwrap();
        // Tighten auth.toml to 0600 so the (correct) loose-perms check stays silent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                global.join("auth.toml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        env.set_evo_dir(&global);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (config, diags) = load_config(&work);
        assert_eq!(config.settings.default_model.as_deref(), Some("m"));
        assert_eq!(config.auth.api_key_entry("anthropic"), Some("sk-x"));
        assert!(diags.is_empty());
    }

    #[test]
    fn drain_renders_warnings() {
        let diags = vec![ConfigDiagnostic::warn("boom", None)];
        let text = drain_diagnostics(&diags);
        assert!(text.contains("boom"));
        assert!(drain_diagnostics(&[]).is_empty());
    }
}
