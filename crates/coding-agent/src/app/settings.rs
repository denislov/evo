use std::fmt;
use std::path::PathBuf;

use crate::app::embedding::CodingAgentThinkingLevel;
use crate::app::operation_factory::CodingAgentOperationFactory;
use crate::config::settings::{
    PartialCompaction, PartialSettings, PartialTerminal, Settings, load_global_settings,
    try_merge_and_save_settings,
};
use crate::config::{SettingsScope, resolve_paths};
use crate::runtime::facade::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};

const MAX_THEME_NAME_CHARS: usize = 128;
const MAX_HTTP_IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentQueueMode {
    #[default]
    OneAtATime,
    All,
}

impl CodingAgentQueueMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneAtATime => "one-at-a-time",
            Self::All => "all",
        }
    }
}

impl fmt::Display for CodingAgentQueueMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for CodingAgentQueueMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "one-at-a-time" => Ok(Self::OneAtATime),
            "all" => Ok(Self::All),
            other => Err(format!("unknown queue mode: {other}")),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentDoubleEscapeAction {
    #[default]
    Tree,
    Fork,
    None,
}

impl CodingAgentDoubleEscapeAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Fork => "fork",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentTreeFilterMode {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentRuntimeSettingsSnapshot {
    pub auto_compaction: bool,
    pub steering_mode: CodingAgentQueueMode,
    pub follow_up_mode: CodingAgentQueueMode,
    pub auto_resize_images: bool,
    pub block_images: bool,
    pub enable_skill_commands: bool,
    pub default_thinking_level: Option<CodingAgentThinkingLevel>,
    pub session_naming_model: Option<String>,
    pub http_idle_timeout_ms: u64,
}

impl Default for CodingAgentRuntimeSettingsSnapshot {
    fn default() -> Self {
        Self {
            auto_compaction: true,
            steering_mode: CodingAgentQueueMode::OneAtATime,
            follow_up_mode: CodingAgentQueueMode::OneAtATime,
            auto_resize_images: true,
            block_images: false,
            enable_skill_commands: true,
            default_thinking_level: None,
            session_naming_model: None,
            http_idle_timeout_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentPresentationSettingsSnapshot {
    pub theme: Option<String>,
    pub show_images: bool,
    pub show_progress: bool,
    pub clear_on_shrink: bool,
    pub hide_thinking_block: bool,
    pub quiet_startup: bool,
    pub double_escape_action: CodingAgentDoubleEscapeAction,
    pub tree_filter_mode: CodingAgentTreeFilterMode,
    pub image_width_cells: u32,
}

impl Default for CodingAgentPresentationSettingsSnapshot {
    fn default() -> Self {
        Self {
            theme: None,
            show_images: true,
            show_progress: false,
            clear_on_shrink: false,
            hide_thinking_block: false,
            quiet_startup: false,
            double_escape_action: CodingAgentDoubleEscapeAction::Tree,
            tree_filter_mode: CodingAgentTreeFilterMode::Default,
            image_width_cells: 60,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingAgentSettingsSnapshot {
    pub runtime: CodingAgentRuntimeSettingsSnapshot,
    pub presentation: CodingAgentPresentationSettingsSnapshot,
}

/// Return the bounded user-global settings projection.
///
/// This reads only the global `settings.toml`. It deliberately does not merge
/// `.evo/settings.toml` from the current working directory or any project.
pub fn global_settings_snapshot() -> CodingAgentSettingsSnapshot {
    snapshot_from_settings(&load_global_settings_state())
}

pub(crate) fn load_global_settings_state() -> Settings {
    let paths = resolve_paths(std::path::Path::new("."));
    let mut diagnostics = Vec::new();
    load_global_settings(&paths, &mut diagnostics)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentSettingsCommand {
    SetTheme(String),
    SetAutoCompaction(bool),
    SetSteeringMode(CodingAgentQueueMode),
    SetFollowUpMode(CodingAgentQueueMode),
    SetProgressVisibility(bool),
    SetImageAutoResize(bool),
    SetImageBlocking(bool),
    SetSkillCommands(bool),
    SetThinkingVisibility(bool),
    SetQuietStartup(bool),
    SetClearOnShrink(bool),
    SetDoubleEscapeAction(CodingAgentDoubleEscapeAction),
    SetDefaultThinkingLevel(CodingAgentThinkingLevel),
    SetSessionNamingModel(String),
    SetHttpIdleTimeoutMs(u64),
}

impl CodingAgentSettingsCommand {
    pub fn set_theme(theme: impl Into<String>) -> Self {
        Self::SetTheme(theme.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSettingsMutationOutcome {
    pub snapshot: CodingAgentSettingsSnapshot,
}

#[derive(Clone)]
pub struct CodingAgentSettingsController {
    cwd: PathBuf,
    settings: Settings,
}

impl fmt::Debug for CodingAgentSettingsController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentSettingsController")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl CodingAgentSettingsController {
    pub(crate) fn from_internal(cwd: impl Into<PathBuf>, settings: Settings) -> Self {
        Self {
            cwd: cwd.into(),
            settings,
        }
    }

    pub fn snapshot(&self) -> CodingAgentSettingsSnapshot {
        snapshot_from_settings(&self.settings)
    }

    pub fn apply(
        &mut self,
        command: CodingAgentSettingsCommand,
        operation_factory: &mut CodingAgentOperationFactory,
    ) -> Result<CodingAgentSettingsMutationOutcome, CodingAgentPublicError> {
        let mut next = self.settings.clone();
        let delta = apply_command(&mut next, command)?;
        try_merge_and_save_settings(&resolve_paths(&self.cwd), SettingsScope::Global, &delta)
            .map_err(|_| settings_persistence_error())?;

        self.settings = next;
        operation_factory.replace_settings(self.settings.clone());
        Ok(CodingAgentSettingsMutationOutcome {
            snapshot: self.snapshot(),
        })
    }
}

fn snapshot_from_settings(settings: &Settings) -> CodingAgentSettingsSnapshot {
    CodingAgentSettingsSnapshot {
        runtime: CodingAgentRuntimeSettingsSnapshot {
            auto_compaction: settings.compaction.enabled,
            steering_mode: queue_mode(&settings.steering_mode),
            follow_up_mode: queue_mode(&settings.follow_up_mode),
            auto_resize_images: settings.terminal.auto_resize_images,
            block_images: settings.terminal.block_images,
            enable_skill_commands: settings.enable_skill_commands,
            default_thinking_level: settings
                .default_thinking_level
                .as_deref()
                .and_then(|value| value.parse().ok()),
            session_naming_model: settings.session_naming_model.clone(),
            http_idle_timeout_ms: settings.http_idle_timeout_ms.min(MAX_HTTP_IDLE_TIMEOUT_MS),
        },
        presentation: CodingAgentPresentationSettingsSnapshot {
            theme: settings
                .theme
                .as_deref()
                .map(|theme| theme.chars().take(MAX_THEME_NAME_CHARS).collect()),
            show_images: settings.terminal.show_images,
            show_progress: settings.terminal.show_progress,
            clear_on_shrink: settings.terminal.clear_on_shrink,
            hide_thinking_block: settings.hide_thinking_block,
            quiet_startup: settings.quiet_startup,
            double_escape_action: double_escape_action(&settings.double_escape_action),
            tree_filter_mode: tree_filter_mode(&settings.tree_filter_mode),
            image_width_cells: settings.terminal.image_width_cells,
        },
    }
}

fn apply_command(
    settings: &mut Settings,
    command: CodingAgentSettingsCommand,
) -> Result<PartialSettings, CodingAgentPublicError> {
    let mut delta = PartialSettings::default();
    match command {
        CodingAgentSettingsCommand::SetTheme(theme) => {
            validate_theme(&theme)?;
            settings.theme = Some(theme.clone());
            delta.theme = Some(theme);
        }
        CodingAgentSettingsCommand::SetAutoCompaction(enabled) => {
            settings.compaction.enabled = enabled;
            delta
                .compaction
                .get_or_insert_with(PartialCompaction::default)
                .enabled = Some(enabled);
        }
        CodingAgentSettingsCommand::SetSteeringMode(mode) => {
            settings.steering_mode = mode.as_str().into();
            delta.steering_mode = Some(mode.as_str().into());
        }
        CodingAgentSettingsCommand::SetFollowUpMode(mode) => {
            settings.follow_up_mode = mode.as_str().into();
            delta.follow_up_mode = Some(mode.as_str().into());
        }
        CodingAgentSettingsCommand::SetProgressVisibility(visible) => {
            settings.terminal.show_progress = visible;
            delta
                .terminal
                .get_or_insert_with(PartialTerminal::default)
                .show_progress = Some(visible);
        }
        CodingAgentSettingsCommand::SetImageAutoResize(enabled) => {
            settings.terminal.auto_resize_images = enabled;
            delta
                .terminal
                .get_or_insert_with(PartialTerminal::default)
                .auto_resize_images = Some(enabled);
        }
        CodingAgentSettingsCommand::SetImageBlocking(enabled) => {
            settings.terminal.block_images = enabled;
            delta
                .terminal
                .get_or_insert_with(PartialTerminal::default)
                .block_images = Some(enabled);
        }
        CodingAgentSettingsCommand::SetSkillCommands(enabled) => {
            settings.enable_skill_commands = enabled;
            delta.enable_skill_commands = Some(enabled);
        }
        CodingAgentSettingsCommand::SetThinkingVisibility(visible) => {
            settings.hide_thinking_block = !visible;
            delta.hide_thinking_block = Some(!visible);
        }
        CodingAgentSettingsCommand::SetQuietStartup(quiet) => {
            settings.quiet_startup = quiet;
            delta.quiet_startup = Some(quiet);
        }
        CodingAgentSettingsCommand::SetClearOnShrink(enabled) => {
            settings.terminal.clear_on_shrink = enabled;
            delta
                .terminal
                .get_or_insert_with(PartialTerminal::default)
                .clear_on_shrink = Some(enabled);
        }
        CodingAgentSettingsCommand::SetDoubleEscapeAction(action) => {
            settings.double_escape_action = action.as_str().into();
            delta.double_escape_action = Some(action.as_str().into());
        }
        CodingAgentSettingsCommand::SetDefaultThinkingLevel(level) => {
            let level = level.to_string();
            settings.default_thinking_level = Some(level.clone());
            delta.default_thinking_level = Some(level);
        }
        CodingAgentSettingsCommand::SetSessionNamingModel(model_id) => {
            let model_id = model_id.trim();
            if model_id.is_empty() || ai::api::model::lookup_model(model_id).is_none() {
                return Err(invalid_settings_command(
                    "session naming model is not available",
                ));
            }
            settings.session_naming_model = Some(model_id.to_owned());
            delta.session_naming_model = Some(model_id.to_owned());
        }
        CodingAgentSettingsCommand::SetHttpIdleTimeoutMs(timeout_ms) => {
            if timeout_ms > MAX_HTTP_IDLE_TIMEOUT_MS {
                return Err(invalid_settings_command(
                    "HTTP idle timeout exceeds the supported limit",
                ));
            }
            settings.http_idle_timeout_ms = timeout_ms;
            delta.http_idle_timeout_ms = Some(timeout_ms);
        }
    }
    Ok(delta)
}

fn queue_mode(value: &str) -> CodingAgentQueueMode {
    match value {
        "all" => CodingAgentQueueMode::All,
        _ => CodingAgentQueueMode::OneAtATime,
    }
}

fn double_escape_action(value: &str) -> CodingAgentDoubleEscapeAction {
    match value {
        "fork" => CodingAgentDoubleEscapeAction::Fork,
        "none" => CodingAgentDoubleEscapeAction::None,
        _ => CodingAgentDoubleEscapeAction::Tree,
    }
}

fn tree_filter_mode(value: &str) -> CodingAgentTreeFilterMode {
    match value {
        "no-tools" => CodingAgentTreeFilterMode::NoTools,
        "user-only" => CodingAgentTreeFilterMode::UserOnly,
        "labeled-only" => CodingAgentTreeFilterMode::LabeledOnly,
        "all" => CodingAgentTreeFilterMode::All,
        _ => CodingAgentTreeFilterMode::Default,
    }
}

fn validate_theme(theme: &str) -> Result<(), CodingAgentPublicError> {
    if theme.is_empty()
        || theme.chars().count() > MAX_THEME_NAME_CHARS
        || theme.chars().any(char::is_control)
    {
        return Err(invalid_settings_command(
            "theme name is empty, too long, or contains control characters",
        ));
    }
    Ok(())
}

fn invalid_settings_command(summary: &str) -> CodingAgentPublicError {
    CodingAgentPublicError {
        category: CodingAgentErrorCategory::Input,
        code: "invalid_settings_command".into(),
        retryable: false,
        summary: summary.into(),
        context: CodingAgentErrorContext::None,
    }
}

fn settings_persistence_error() -> CodingAgentPublicError {
    CodingAgentPublicError {
        category: CodingAgentErrorCategory::Persistence,
        code: "settings_persistence".into(),
        retryable: true,
        summary: "failed to update product settings".into(),
        context: CodingAgentErrorContext::None,
    }
}
