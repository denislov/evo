use crate::config::storage::{atomic_write_private, read_bounded_text};
use crate::config::{ConfigDiagnostic, ConfigPaths};
use agent_core::api::agent::MAX_COMPACTION_TOKEN_BUDGET;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

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
pub struct PartialWarnings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_extra_usage: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialTerminal {
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
    pub transport: Option<String>,
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
    pub collapse_changelog: Option<bool>,
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
    pub npm_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_idle_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_connect_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<PartialWarnings>,
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
    pub http_idle_timeout_ms: u64,
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

fn merge_warnings(
    base: Option<PartialWarnings>,
    over: Option<PartialWarnings>,
) -> Option<PartialWarnings> {
    match (base, over) {
        (None, x) | (x, None) => x,
        (Some(b), Some(o)) => Some(PartialWarnings {
            anthropic_extra_usage: o.anthropic_extra_usage.or(b.anthropic_extra_usage),
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
            transport: over.transport.or(self.transport),
            steering_mode: over.steering_mode.or(self.steering_mode),
            follow_up_mode: over.follow_up_mode.or(self.follow_up_mode),
            session_dir: over.session_dir.or(self.session_dir),
            skills: merge_vec(self.skills, over.skills),
            prompts: merge_vec(self.prompts, over.prompts),
            themes: merge_vec(self.themes, over.themes),
            theme: over.theme.or(self.theme),
            no_context_files: over.no_context_files.or(self.no_context_files),
            hide_thinking_block: over.hide_thinking_block.or(self.hide_thinking_block),
            collapse_changelog: over.collapse_changelog.or(self.collapse_changelog),
            quiet_startup: over.quiet_startup.or(self.quiet_startup),
            enable_skill_commands: over.enable_skill_commands.or(self.enable_skill_commands),
            double_escape_action: over.double_escape_action.or(self.double_escape_action),
            tree_filter_mode: over.tree_filter_mode.or(self.tree_filter_mode),
            shell_path: over.shell_path.or(self.shell_path),
            shell_command_prefix: over.shell_command_prefix.or(self.shell_command_prefix),
            npm_command: merge_vec(self.npm_command, over.npm_command),
            http_proxy: over.http_proxy.or(self.http_proxy),
            http_idle_timeout_ms: over.http_idle_timeout_ms.or(self.http_idle_timeout_ms),
            websocket_connect_timeout_ms: over
                .websocket_connect_timeout_ms
                .or(self.websocket_connect_timeout_ms),
            enabled_models: merge_vec(self.enabled_models, over.enabled_models),
            warnings: merge_warnings(self.warnings, over.warnings),
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
            http_idle_timeout_ms: self.http_idle_timeout_ms.unwrap_or(300000),
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

/// Merge a `PartialSettings` delta into the settings file for the given
/// scope and write it back to disk. Only non-`None` fields in `delta`
/// overwrite matching keys in the file; `None` fields are left untouched.
/// Creates the file if it doesn't exist.
#[cfg(test)]
pub fn merge_and_save_settings(
    paths: &ConfigPaths,
    scope: SettingsScope,
    delta: &PartialSettings,
    diags: &mut Vec<ConfigDiagnostic>,
) {
    if let Err(diagnostic) = try_merge_and_save_settings(paths, scope, delta) {
        diags.push(diagnostic);
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
            diagnose_unsupported_settings(&parsed, path, diags);
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

fn diagnose_unsupported_settings(
    settings: &PartialSettings,
    path: &Path,
    diags: &mut Vec<ConfigDiagnostic>,
) {
    let mut warn = |key: &str| {
        diags.push(ConfigDiagnostic::warn(
            format!("settings key `{key}` is recognized but not supported by the Rust runtime"),
            Some(path.to_path_buf()),
        ));
    };
    if settings.collapse_changelog.is_some() {
        warn("collapse_changelog");
    }
    if settings.transport.is_some() {
        warn("transport");
    }
    if settings.npm_command.is_some() {
        warn("npm_command");
    }
    if settings.http_proxy.is_some() {
        warn("http_proxy");
    }
    if settings.websocket_connect_timeout_ms.is_some() {
        warn("websocket_connect_timeout_ms");
    }
    if settings
        .warnings
        .as_ref()
        .is_some_and(|warnings| warnings.anthropic_extra_usage.is_some())
    {
        warn("warnings.anthropic_extra_usage");
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
    fn defaults_applied_on_empty() {
        let s = PartialSettings::default().resolve();
        assert_eq!(s.steering_mode, "one-at-a-time");
        assert!(s.compaction.enabled);
        assert_eq!(s.compaction.reserve_tokens, 16384);
        assert_eq!(s.compaction.keep_recent_tokens, 20000);
        assert_eq!(s.retry.max_retries, 3);
        assert_eq!(s.retry.base_delay_ms, 2000);
        assert!(s.default_model.is_none());
        assert!(!s.hide_thinking_block);
        assert!(!s.quiet_startup);
        assert!(s.enable_skill_commands);
        assert_eq!(s.double_escape_action, "tree");
        assert_eq!(s.tree_filter_mode, "default");
        assert_eq!(s.terminal.mode, TuiMode::Inline);
        assert!(!s.terminal.clear_on_shrink);
        assert!(s.terminal.auto_resize_images);
        assert!(!s.terminal.block_images);
        assert_eq!(s.terminal.image_width_cells, 60);
        assert!(s.shell_path.is_none());
        assert!(s.shell_command_prefix.is_none());
        assert_eq!(s.http_idle_timeout_ms, 300000);
        assert!(s.enabled_models.is_empty());
    }

    #[test]
    fn compaction_budget_rejects_truncating_and_aggregate_extremes() {
        assert!(
            toml::from_str::<PartialSettings>(
                "[compaction]\nreserve_tokens = 18446744073709551615\n"
            )
            .is_err(),
            "values outside u32 must fail instead of truncating through `as`"
        );

        let mut settings = PartialSettings {
            compaction: Some(PartialCompaction {
                enabled: Some(true),
                reserve_tokens: Some(MAX_COMPACTION_TOKEN_BUDGET),
                keep_recent_tokens: Some(1),
            }),
            ..Default::default()
        };
        let mut diagnostics = Vec::new();
        validate_compaction_settings(&mut settings, None, true, &mut diagnostics);

        assert!(settings.compaction.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("ignored invalid compaction token budget")
        );
    }

    #[test]
    fn terminal_mode_is_typed_and_project_overridable() {
        let global: PartialSettings = toml::from_str("[terminal]\nmode = \"fullscreen\"\n")
            .expect("fullscreen terminal mode should parse");
        let project: PartialSettings = toml::from_str("[terminal]\nmode = \"inline\"\n")
            .expect("inline terminal mode should parse");

        assert_eq!(
            global.merge(project).resolve().terminal.mode,
            TuiMode::Inline
        );
        assert!(toml::from_str::<PartialSettings>("[terminal]\nmode = \"windowed\"\n").is_err());
    }

    #[test]
    fn project_overrides_global_scalars() {
        let global = PartialSettings {
            default_model: Some("a".into()),
            default_thinking_level: Some("high".into()),
            ..Default::default()
        };
        let project = PartialSettings {
            default_model: Some("b".into()),
            ..Default::default()
        };
        let s = global.merge(project).resolve();
        assert_eq!(s.default_model.as_deref(), Some("b")); // project wins
        assert_eq!(s.default_thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn nested_objects_merge_field_wise() {
        let global = PartialSettings {
            compaction: Some(PartialCompaction {
                reserve_tokens: Some(100),
                keep_recent_tokens: Some(200),
                ..Default::default()
            }),
            ..Default::default()
        };
        let project = PartialSettings {
            compaction: Some(PartialCompaction {
                reserve_tokens: Some(999),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = global.merge(project).resolve();
        assert_eq!(s.compaction.reserve_tokens, 999); // project overrides
        assert_eq!(s.compaction.keep_recent_tokens, 200); // global field survives
        assert!(s.compaction.enabled); // default fills the gap
    }

    #[test]
    fn terminal_theme_and_context_settings_resolve_defaults_and_merge() {
        let global = PartialSettings {
            theme: Some("dark".into()),
            no_context_files: Some(true),
            terminal: Some(PartialTerminal {
                show_images: Some(false),
                show_progress: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let project = PartialSettings {
            theme: Some("light".into()),
            terminal: Some(PartialTerminal {
                show_progress: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        let s = global.merge(project).resolve();

        assert_eq!(s.theme.as_deref(), Some("light"));
        assert!(s.no_context_files);
        assert!(!s.terminal.show_images);
        assert!(s.terminal.show_progress);
    }

    #[test]
    fn terminal_new_fields_merge_field_wise() {
        let global = PartialSettings {
            terminal: Some(PartialTerminal {
                clear_on_shrink: Some(true),
                auto_resize_images: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let project = PartialSettings {
            terminal: Some(PartialTerminal {
                auto_resize_images: Some(true),
                block_images: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = global.merge(project).resolve();
        assert!(s.terminal.clear_on_shrink); // global survives
        assert!(s.terminal.auto_resize_images); // project overrides
        assert!(s.terminal.block_images); // project adds
    }

    #[test]
    fn scalar_new_fields_merge_and_resolve() {
        let global = PartialSettings {
            hide_thinking_block: Some(true),
            quiet_startup: Some(true),
            double_escape_action: Some("fork".into()),
            ..Default::default()
        };
        let project = PartialSettings {
            hide_thinking_block: Some(false),
            collapse_changelog: Some(true),
            tree_filter_mode: Some("user-only".into()),
            ..Default::default()
        };
        let s = global.merge(project).resolve();
        assert!(!s.hide_thinking_block); // project overrides
        assert!(s.quiet_startup); // global survives
        assert!(s.enable_skill_commands); // default
        assert_eq!(s.double_escape_action, "fork"); // global survives
        assert_eq!(s.tree_filter_mode, "user-only"); // project overrides
    }

    #[test]
    fn enabled_models_merge() {
        let global = PartialSettings {
            enabled_models: Some(vec!["claude-*".into()]),
            ..Default::default()
        };
        let project = PartialSettings {
            enabled_models: Some(vec!["gpt-4*".into()]),
            ..Default::default()
        };
        let s = global.merge(project).resolve();
        assert_eq!(s.enabled_models, vec!["claude-*", "gpt-4*"]);
    }

    #[test]
    fn terminal_defaults_are_enabled_and_context_files_default_on() {
        let s = PartialSettings::default().resolve();
        assert!(s.terminal.show_images);
        assert!(!s.terminal.show_progress);
        assert!(!s.terminal.clear_on_shrink);
        assert!(s.terminal.auto_resize_images);
        assert!(!s.terminal.block_images);
        assert!(!s.no_context_files);
        assert!(!s.hide_thinking_block);
        assert!(s.enable_skill_commands);
        assert_eq!(s.double_escape_action, "tree");
        assert_eq!(s.tree_filter_mode, "default");
        assert!(s.theme.is_none());
    }

    #[test]
    fn missing_file_yields_default_no_diags() {
        let mut diags = Vec::new();
        let p = std::path::Path::new("/nonexistent/dir/settings.toml");
        let parsed = load_partial(p, &mut diags);
        assert_eq!(parsed, PartialSettings::default());
        assert!(diags.is_empty());
    }

    #[test]
    fn parses_toml_and_unknown_field_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "default_model = \"x\"\nbogus_field = 1\n").unwrap();
        let mut diags = Vec::new();
        let parsed = load_partial(&path, &mut diags);
        // deny_unknown_fields makes the whole parse fail -> default + warn diagnostic
        assert_eq!(parsed, PartialSettings::default());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, crate::config::DiagnosticSeverity::Warn);
    }

    #[test]
    fn load_settings_project_overrides_global() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            global.path().join("settings.toml"),
            "default_model = \"g\"\ntransport = \"sse\"\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("settings.toml"),
            "default_model = \"p\"\n",
        )
        .unwrap();
        let paths = crate::config::ConfigPaths {
            global_dir: global.path().to_path_buf(),
            project_dir: project.path().to_path_buf(),
        };
        let mut diags = Vec::new();
        let s = load_settings(&paths, &mut diags);
        assert_eq!(s.default_model.as_deref(), Some("p"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("`transport`"));
    }

    #[test]
    fn resource_paths_merge_global_and_project_settings() {
        let global = PartialSettings {
            skills: Some(vec!["global-skill".into()]),
            prompts: Some(vec!["global-prompt".into()]),
            themes: Some(vec!["global-theme".into()]),
            ..Default::default()
        };
        let project = PartialSettings {
            skills: Some(vec!["project-skill".into()]),
            prompts: Some(vec!["project-prompt".into()]),
            themes: Some(vec!["project-theme".into()]),
            ..Default::default()
        };

        let s = global.merge(project).resolve();

        assert_eq!(s.skills, vec!["global-skill", "project-skill"]);
        assert_eq!(s.prompts, vec!["global-prompt", "project-prompt"]);
        assert_eq!(s.themes, vec!["global-theme", "project-theme"]);
    }

    #[test]
    fn merge_and_save_settings_writes_delta_and_preserves_existing() {
        use crate::config::ConfigPaths;
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();

        // Write an existing settings file with a field we won't touch
        std::fs::write(
            global.join("settings.toml"),
            "default_model = \"claude-3\"\ntransport = \"sse\"\n",
        )
        .unwrap();

        let paths = ConfigPaths {
            global_dir: global.clone(),
            project_dir: project.clone(),
        };
        let delta = PartialSettings {
            theme: Some("light".into()),
            ..Default::default()
        };

        let mut diags = Vec::new();
        crate::config::settings::merge_and_save_settings(
            &paths,
            crate::config::SettingsScope::Global,
            &delta,
            &mut diags,
        );

        assert!(diags.is_empty(), "diags: {diags:?}");

        // Read back and verify merge
        let saved = std::fs::read_to_string(global.join("settings.toml")).unwrap();
        let parsed: PartialSettings = toml::from_str(&saved).unwrap();
        assert_eq!(
            parsed.default_model.as_deref(),
            Some("claude-3"),
            "existing field preserved"
        );
        assert_eq!(
            parsed.transport.as_deref(),
            Some("sse"),
            "existing field preserved"
        );
        assert_eq!(
            parsed.theme.as_deref(),
            Some("light"),
            "delta field written"
        );
    }

    #[test]
    fn merge_and_save_settings_creates_file_when_missing() {
        use crate::config::ConfigPaths;
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        let paths = ConfigPaths {
            global_dir: global.clone(),
            project_dir: dir.path().join("project"),
        };
        let delta = PartialSettings {
            theme: Some("dark".into()),
            ..Default::default()
        };
        let mut diags = Vec::new();
        merge_and_save_settings(&paths, SettingsScope::Global, &delta, &mut diags);
        assert!(diags.is_empty());
        let saved = std::fs::read_to_string(global.join("settings.toml")).unwrap();
        assert!(saved.contains("dark"));
    }

    #[test]
    fn merge_and_save_settings_handles_nested_delta_merge() {
        use crate::config::ConfigPaths;
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            "[compaction]\nenabled = true\nreserve_tokens = 16384\n",
        )
        .unwrap();
        let paths = ConfigPaths {
            global_dir: global.clone(),
            project_dir: dir.path().join("project"),
        };
        // Only change compaction.enabled
        let delta = PartialSettings {
            compaction: Some(PartialCompaction {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut diags = Vec::new();
        merge_and_save_settings(&paths, SettingsScope::Global, &delta, &mut diags);
        assert!(diags.is_empty());
        let saved = std::fs::read_to_string(global.join("settings.toml")).unwrap();
        let parsed: PartialSettings = toml::from_str(&saved).unwrap();
        let c = parsed.compaction.unwrap();
        assert!(!c.enabled.unwrap(), "delta overrides");
        assert_eq!(c.reserve_tokens, Some(16384), "existing field preserved");
    }

    #[test]
    fn partial_settings_serialize_round_trip() {
        let original = PartialSettings {
            theme: Some("light".into()),
            transport: Some("sse".into()),
            compaction: Some(PartialCompaction {
                enabled: Some(false),
                reserve_tokens: Some(8192),
                ..Default::default()
            }),
            terminal: Some(PartialTerminal {
                show_images: Some(false),
                image_width_cells: Some(80),
                ..Default::default()
            }),
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&original).expect("serialize should succeed");
        let parsed: PartialSettings =
            toml::from_str(&toml_str).expect("deserialize should succeed");
        assert_eq!(original, parsed);
    }

    #[test]
    fn unsupported_settings_emit_key_only_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            r#"
collapse_changelog = true
transport = "websocket"
npm_command = ["pnpm"]
http_proxy = "http://user:secret@example.invalid:8080"
websocket_connect_timeout_ms = 1234

[warnings]
anthropic_extra_usage = false
"#,
        )
        .unwrap();

        let mut diagnostics = Vec::new();
        let parsed = load_partial(&path, &mut diagnostics);

        assert_eq!(parsed.npm_command, Some(vec!["pnpm".into()]));
        assert_eq!(diagnostics.len(), 6);
        let rendered = crate::config::drain_diagnostics(&diagnostics);
        for key in [
            "collapse_changelog",
            "transport",
            "npm_command",
            "http_proxy",
            "websocket_connect_timeout_ms",
            "warnings.anthropic_extra_usage",
        ] {
            assert!(rendered.contains(key), "missing diagnostic for {key}");
        }
        assert!(!rendered.contains("user"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("example.invalid"));
    }
}
