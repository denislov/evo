use std::path::{Path, PathBuf};

pub struct ConfigPaths {
    pub global_dir: PathBuf,
    pub project_dir: PathBuf,
}

impl ConfigPaths {
    pub fn global_settings(&self) -> PathBuf {
        self.global_dir.join("settings.toml")
    }
    pub fn project_settings(&self) -> PathBuf {
        self.project_dir.join("settings.toml")
    }
    pub fn global_auth(&self) -> PathBuf {
        self.global_dir.join("auth.toml")
    }
}

pub fn resolve(cwd: &Path) -> ConfigPaths {
    let global_dir = match std::env::var_os("EVO_DIR") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".evo"),
    };
    ConfigPaths {
        global_dir,
        project_dir: cwd.join(".evo"),
    }
}
