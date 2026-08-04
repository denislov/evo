use crate::config::storage::{atomic_write_private, read_bounded_text};
use crate::config::{ConfigDiagnostic, ConfigPaths};
use agent_core::api::agent::MAX_COMPACTION_TOKEN_BUDGET;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Legacy `[terminal] mode` configuration value.
///
/// The interactive TUI is always fullscreen now; this enum only exists so
/// existing settings files keep parsing. `Inline` is accepted (and ignored)
/// for backward compatibility with older configs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiMode {
    #[default]
    Inline,
    Fullscreen,
}

impl FromStr for TuiMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inline" => Ok(Self::Inline),
            "fullscreen" => Ok(Self::Fullscreen),
            other => Err(format!("unknown TUI mode: {other}")),
        }
    }
}

/// Which settings file to target when saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    Global,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialCompaction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<u32>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialRetry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_delay_ms: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialTerminal {
    /// Legacy mode selector; accepted for config compatibility and otherwise
    /// ignored (the interactive TUI always owns the full screen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<TuiMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_on_shrink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_resize_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_width_cells: Option<u32>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_naming_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steering_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_context_files: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_thinking_block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_startup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_skill_commands: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_escape_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_filter_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_command_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_idle_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_connect_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<PartialTerminal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<PartialCompaction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<PartialRetry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u32,
    pub keep_recent_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSettings {
    pub mode: TuiMode,
    pub show_images: bool,
    pub show_progress: bool,
    pub clear_on_shrink: bool,
    pub auto_resize_images: bool,
    pub block_images: bool,
    pub image_width_cells: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<String>,
    pub session_naming_model: Option<String>,
    pub steering_mode: String,
    pub follow_up_mode: String,
    pub session_dir: Option<String>,
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub themes: Vec<String>,
    pub theme: Option<String>,
    pub no_context_files: bool,
    pub hide_thinking_block: bool,
    pub quiet_startup: bool,
    pub enable_skill_commands: bool,
    pub double_escape_action: String,
    pub tree_filter_mode: String,
    pub shell_path: Option<String>,
    pub shell_command_prefix: Option<String>,
    pub http_proxy: Option<String>,
    pub http_idle_timeout_ms: u64,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub enabled_models: Vec<String>,
    pub terminal: TerminalSettings,
    pub compaction: CompactionSettings,
    pub retry: RetrySettings,
}

fn merge_compaction(
    base: Option<PartialCompaction>,
    over: Option<PartialCompaction>,
) -> Option<PartialCompaction> {
    match (base, over) {
        (None, x) | (x, None) => x,
        (Some(b), Some(o)) => Some(PartialCompaction {
            enabled: o.enabled.or(b.enabled),
            reserve_tokens: o.reserve_tokens.or(b.reserve_tokens),
            keep_recent_tokens: o.keep_recent_tokens.or(b.keep_recent_tokens),
        }),
    }
}

fn merge_retry(base: Option<PartialRetry>, over: Option<PartialRetry>) -> Option<PartialRetry> {
    match (base, over) {
        (None, x) | (x, None) => x,
        (Some(b), Some(o)) => Some(PartialRetry {
            enabled: o.enabled.or(b.enabled),
            max_retries: o.max_retries.or(b.max_retries),
            base_delay_ms: o.base_delay_ms.or(b.base_delay_ms),
        }),
    }
}

fn merge_terminal(
    base: Option<PartialTerminal>,
    over: Option<PartialTerminal>,
) -> Option<PartialTerminal> {
    match (base, over) {
        (None, x) | (x, None) => x,
        (Some(b), Some(o)) => Some(PartialTerminal {
            mode: o.mode.or(b.mode),
            show_images: o.show_images.or(b.show_images),
            show_progress: o.show_progress.or(b.show_progress),
            clear_on_shrink: o.clear_on_shrink.or(b.clear_on_shrink),
            auto_resize_images: o.auto_resize_images.or(b.auto_resize_images),
            block_images: o.block_images.or(b.block_images),
            image_width_cells: o.image_width_cells.or(b.image_width_cells),
        }),
    }
}

fn merge_vec(base: Option<Vec<String>>, over: Option<Vec<String>>) -> Option<Vec<String>> {
    match (base, over) {
        (None, x) | (x, None) => x,
        (Some(mut base), Some(over)) => {
            base.extend(over);
            Some(base)
        }
    }
}

impl PartialSettings {
    pub fn merge(self, over: PartialSettings) -> PartialSettings {
        PartialSettings {
            default_provider: over.default_provider.or(self.default_provider),
            default_model: over.default_model.or(self.default_model),
            default_thinking_level: over.default_thinking_level.or(self.default_thinking_level),
            session_naming_model: over.session_naming_model.or(self.session_naming_model),
            steering_mode: over.steering_mode.or(self.steering_mode),
            follow_up_mode: over.follow_up_mode.or(self.follow_up_mode),
            session_dir: over.session_dir.or(self.session_dir),
            skills: merge_vec(self.skills, over.skills),
            prompts: merge_vec(self.prompts, over.prompts),
            themes: merge_vec(self.themes, over.themes),
            theme: over.theme.or(self.theme),
            no_context_files: over.no_context_files.or(self.no_context_files),
            hide_thinking_block: over.hide_thinking_block.or(self.hide_thinking_block),
            quiet_startup: over.quiet_startup.or(self.quiet_startup),
            enable_skill_commands: over.enable_skill_commands.or(self.enable_skill_commands),
            double_escape_action: over.double_escape_action.or(self.double_escape_action),
            tree_filter_mode: over.tree_filter_mode.or(self.tree_filter_mode),
            shell_path: over.shell_path.or(self.shell_path),
            shell_command_prefix: over.shell_command_prefix.or(self.shell_command_prefix),
            http_proxy: over.http_proxy.or(self.http_proxy),
            http_idle_timeout_ms: over.http_idle_timeout_ms.or(self.http_idle_timeout_ms),
            websocket_connect_timeout_ms: over
                .websocket_connect_timeout_ms
                .or(self.websocket_connect_timeout_ms),
            enabled_models: merge_vec(self.enabled_models, over.enabled_models),
            terminal: merge_terminal(self.terminal, over.terminal),
            compaction: merge_compaction(self.compaction, over.compaction),
            retry: merge_retry(self.retry, over.retry),
        }
    }

    pub fn resolve(self) -> Settings {
        let c = self.compaction.unwrap_or_default();
        let r = self.retry.unwrap_or_default();
        let t = self.terminal.unwrap_or_default();
        Settings {
            default_provider: self.default_provider,
            default_model: self.default_model,
            default_thinking_level: self.default_thinking_level,
            session_naming_model: self.session_naming_model,
            steering_mode: self
                .steering_mode
                .unwrap_or_else(|| "one-at-a-time".to_string()),
            follow_up_mode: self
                .follow_up_mode
                .unwrap_or_else(|| "one-at-a-time".to_string()),
            session_dir: self.session_dir,
            skills: self.skills.unwrap_or_default(),
            prompts: self.prompts.unwrap_or_default(),
            themes: self.themes.unwrap_or_default(),
            theme: self.theme,
            no_context_files: self.no_context_files.unwrap_or(false),
            hide_thinking_block: self.hide_thinking_block.unwrap_or(false),
            quiet_startup: self.quiet_startup.unwrap_or(false),
            enable_skill_commands: self.enable_skill_commands.unwrap_or(true),
            double_escape_action: self
                .double_escape_action
                .unwrap_or_else(|| "tree".to_string()),
            tree_filter_mode: self
                .tree_filter_mode
                .unwrap_or_else(|| "default".to_string()),
            shell_path: self.shell_path,
            shell_command_prefix: self.shell_command_prefix,
            http_proxy: self.http_proxy.and_then(|proxy| {
                let proxy = proxy.trim();
                (!proxy.is_empty()).then(|| proxy.to_owned())
            }),
            http_idle_timeout_ms: self.http_idle_timeout_ms.unwrap_or(300000),
            websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
            enabled_models: self.enabled_models.unwrap_or_default(),
            terminal: TerminalSettings {
                mode: t.mode.unwrap_or_default(),
                show_images: t.show_images.unwrap_or(true),
                show_progress: t.show_progress.unwrap_or(false),
                clear_on_shrink: t.clear_on_shrink.unwrap_or(false),
                auto_resize_images: t.auto_resize_images.unwrap_or(true),
                block_images: t.block_images.unwrap_or(false),
                image_width_cells: t.image_width_cells.unwrap_or(60),
            },
            compaction: CompactionSettings {
                enabled: c.enabled.unwrap_or(true),
                reserve_tokens: c.reserve_tokens.unwrap_or(16384),
                keep_recent_tokens: c.keep_recent_tokens.unwrap_or(20000),
            },
            retry: RetrySettings {
                enabled: r.enabled.unwrap_or(true),
                max_retries: r.max_retries.unwrap_or(3),
                base_delay_ms: r.base_delay_ms.unwrap_or(2000),
            },
        }
    }
}

/// Recursively merge `over` table into `base` table. `over` overwrites `base`.
fn merge_toml_tables(base: &mut toml::value::Table, over: &toml::value::Table) {
    for (key, value) in over {
        match (base.get_mut(key), value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(over_table)) => {
                merge_toml_tables(base_table, over_table);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

pub(crate) fn try_merge_and_save_settings(
    paths: &ConfigPaths,
    scope: SettingsScope,
    delta: &PartialSettings,
) -> Result<(), ConfigDiagnostic> {
    let path = match scope {
        SettingsScope::Global => paths.global_settings(),
    };

    // Serialize delta to TOML string, then parse as Value::Table.
    // Because PartialSettings uses skip_serializing_if = Option::is_none,
    // only fields that are Some(...) appear in the output.
    let delta_str = match toml::to_string(delta) {
        Ok(s) => s,
        Err(err) => {
            return Err(ConfigDiagnostic::warn(
                format!("failed to serialize settings delta: {err}"),
                Some(path),
            ));
        }
    };
    let delta_value: toml::Value = match toml::from_str(&delta_str) {
        Ok(v) => v,
        Err(err) => {
            return Err(ConfigDiagnostic::warn(
                format!("failed to parse serialized delta: {err}"),
                Some(path),
            ));
        }
    };
    let Some(delta_table) = delta_value.as_table() else {
        return Err(ConfigDiagnostic::warn(
            "settings delta produced a non-table value".to_string(),
            Some(path),
        ));
    };

    // Read existing file content, or start with an empty table
    let mut current_table = match read_bounded_text(&path) {
        Ok(Some(text)) => toml::from_str::<toml::Value>(&text)
            .ok()
            .and_then(|v| match v {
                toml::Value::Table(t) => Some(t),
                _ => None,
            })
            .unwrap_or_default(),
        Ok(None) => toml::value::Table::new(),
        Err(err) => {
            return Err(ConfigDiagnostic::warn(
                format!("failed to read settings file: {err}"),
                Some(path),
            ));
        }
    };

    // Merge delta into current
    merge_toml_tables(&mut current_table, delta_table);

    // Serialize merged table and write
    let merged_value = toml::Value::Table(current_table);
    let merged_str = match toml::to_string_pretty(&merged_value) {
        Ok(s) => s,
        Err(err) => {
            return Err(ConfigDiagnostic::warn(
                format!("failed to serialize merged settings: {err}"),
                Some(path),
            ));
        }
    };

    atomic_write_private(&path, merged_str.as_bytes()).map_err(|err| {
        ConfigDiagnostic::warn(format!("failed to write settings file: {err}"), Some(path))
    })
}

pub fn load_partial(path: &Path, diags: &mut Vec<ConfigDiagnostic>) -> PartialSettings {
    let text = match read_bounded_text(path) {
        Ok(Some(text)) => text,
        Ok(None) => {
            return PartialSettings::default();
        }
        Err(err) => {
            diags.push(ConfigDiagnostic::warn(
                format!("failed to read settings: {err}"),
                Some(path.to_path_buf()),
            ));
            return PartialSettings::default();
        }
    };
    match toml::from_str::<PartialSettings>(&text) {
        Ok(mut parsed) => {
            validate_compaction_settings(&mut parsed, Some(path), false, diags);
            parsed
        }
        Err(err) => {
            diags.push(ConfigDiagnostic::warn(
                format!("failed to parse settings: {err}"),
                Some(path.to_path_buf()),
            ));
            PartialSettings::default()
        }
    }
}

fn validate_compaction_settings(
    settings: &mut PartialSettings,
    path: Option<&Path>,
    apply_defaults: bool,
    diags: &mut Vec<ConfigDiagnostic>,
) {
    let Some(compaction) = settings.compaction.as_ref() else {
        return;
    };
    let reserve_tokens = compaction
        .reserve_tokens
        .or(apply_defaults.then_some(16_384));
    let keep_recent_tokens = compaction
        .keep_recent_tokens
        .or(apply_defaults.then_some(20_000));
    let invalid = reserve_tokens.is_some_and(|value| value > MAX_COMPACTION_TOKEN_BUDGET)
        || keep_recent_tokens.is_some_and(|value| value > MAX_COMPACTION_TOKEN_BUDGET)
        || reserve_tokens
            .zip(keep_recent_tokens)
            .is_some_and(|(reserve, keep)| {
                u64::from(reserve) + u64::from(keep) > u64::from(MAX_COMPACTION_TOKEN_BUDGET)
            });
    if invalid {
        diags.push(ConfigDiagnostic::warn(
            format!(
                "ignored invalid compaction token budget; reserve_tokens + \
                 keep_recent_tokens must be at most {MAX_COMPACTION_TOKEN_BUDGET}"
            ),
            path.map(Path::to_path_buf),
        ));
        settings.compaction = None;
    }
}

pub fn load_settings(paths: &ConfigPaths, diags: &mut Vec<ConfigDiagnostic>) -> Settings {
    let global = load_partial(&paths.global_settings(), diags);
    let project = load_partial(&paths.project_settings(), diags);
    let mut merged = global.merge(project);
    validate_compaction_settings(&mut merged, None, true, diags);
    merged.resolve()
}

/// Load only the user-global settings file, without consulting project state.
pub(crate) fn load_global_settings(
    paths: &ConfigPaths,
    diags: &mut Vec<ConfigDiagnostic>,
) -> Settings {
    let mut global = load_partial(&paths.global_settings(), diags);
    validate_compaction_settings(&mut global, None, true, diags);
    global.resolve()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_settings_survive_parse_merge_and_resolve() {
        let global: PartialSettings = toml::from_str(
            "http_proxy = 'http://127.0.0.1:8080'\nwebsocket_connect_timeout_ms = 4500\n",
        )
        .expect("parse transport settings");
        let project: PartialSettings = toml::from_str("websocket_connect_timeout_ms = 9000\n")
            .expect("parse project transport settings");
        let resolved = global.merge(project).resolve();
        assert_eq!(
            resolved.http_proxy.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(resolved.websocket_connect_timeout_ms, Some(9000));
    }

    #[test]
    fn removed_typescript_settings_are_rejected_by_the_schema() {
        for legacy in [
            "transport = 'sse'\n",
            "npm_command = ['npm']\n",
            "collapse_changelog = true\n",
            "[warnings]\nanthropic_extra_usage = true\n",
        ] {
            assert!(
                toml::from_str::<PartialSettings>(legacy).is_err(),
                "legacy setting unexpectedly remained in schema: {legacy}"
            );
        }
    }
}
