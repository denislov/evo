use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::resources::ThemeResource;
use crate::runtime::facade::{CodingAgentPublicError, CodingSessionError};
use crate::theme::{
    ResolvedColor, ResolvedTheme, ThemeBg, ThemeColor, ThemeReloadSignal, ThemeWatcher,
};

/// UI-neutral concrete color exposed to product adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentResolvedColor {
    Default,
    Rgb(u8, u8, u8),
    Ansi256(u8),
}

/// Stable semantic foreground roles in a resolved coding-agent theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingAgentThemeForeground {
    Accent,
    Border,
    BorderAccent,
    BorderMuted,
    Success,
    Error,
    Warning,
    Muted,
    Dim,
    Text,
    ThinkingText,
    UserMessageText,
    CustomMessageText,
    CustomMessageLabel,
    ToolTitle,
    ToolOutput,
    MdHeading,
    MdLink,
    MdLinkUrl,
    MdCode,
    MdCodeBlock,
    MdCodeBlockBorder,
    MdQuote,
    MdQuoteBorder,
    MdHr,
    MdListBullet,
    ToolDiffAdded,
    ToolDiffRemoved,
    ToolDiffContext,
    SyntaxComment,
    SyntaxKeyword,
    SyntaxFunction,
    SyntaxVariable,
    SyntaxString,
    SyntaxNumber,
    SyntaxType,
    SyntaxOperator,
    SyntaxPunctuation,
    ThinkingOff,
    ThinkingMinimal,
    ThinkingLow,
    ThinkingMedium,
    ThinkingHigh,
    ThinkingXhigh,
    BashMode,
}

/// Stable semantic background roles in a resolved coding-agent theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodingAgentThemeBackground {
    Selected,
    UserMessage,
    CustomMessage,
    ToolPending,
    ToolSuccess,
    ToolError,
}

/// Fully resolved, authority-free theme projection for presentation adapters.
///
/// Raw JSON, variables, schema details, source paths, and file-loading
/// authority remain private to the product crate.
#[derive(Clone, PartialEq, Eq)]
pub struct CodingAgentThemeSnapshot {
    name: String,
    resolved: ResolvedTheme,
}

impl fmt::Debug for CodingAgentThemeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentThemeSnapshot")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl CodingAgentThemeSnapshot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn foreground(&self, role: CodingAgentThemeForeground) -> CodingAgentResolvedColor {
        resolved_color(self.resolved.fg(foreground_role(role)))
    }

    pub fn background(&self, role: CodingAgentThemeBackground) -> CodingAgentResolvedColor {
        resolved_color(self.resolved.bg(background_role(role)))
    }

    pub fn dark() -> Self {
        Self::from_resolved(
            "dark".into(),
            crate::theme::builtin_dark()
                .resolve_colors()
                .expect("built-in dark theme resolves"),
        )
    }

    pub fn light() -> Self {
        Self::from_resolved(
            "light".into(),
            crate::theme::builtin_light()
                .resolve_colors()
                .expect("built-in light theme resolves"),
        )
    }

    fn from_resolved(name: String, resolved: ResolvedTheme) -> Self {
        Self { name, resolved }
    }

    pub(crate) fn from_resource(resource: &ThemeResource) -> Result<Self, CodingAgentPublicError> {
        resource
            .theme
            .resolve_colors()
            .map(|resolved| Self::from_resolved(resource.name.clone(), resolved))
            .map_err(|error| theme_error(format!("theme could not be resolved: {error:?}")))
    }

    fn from_reload(reload: ThemeReloadSignal) -> Result<Self, CodingAgentPublicError> {
        reload
            .theme
            .resolve_colors()
            .map(|resolved| Self::from_resolved(reload.name, resolved))
            .map_err(|error| {
                theme_error(format!("reloaded theme could not be resolved: {error:?}"))
            })
    }
}

/// Product-owned access to theme selection and hot reload.
///
/// The handle intentionally omits its filesystem root from `Debug` and from
/// every public return value.
#[derive(Clone)]
pub struct CodingAgentThemeController {
    themes_dir: PathBuf,
}

impl fmt::Debug for CodingAgentThemeController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentThemeController")
            .finish_non_exhaustive()
    }
}

impl CodingAgentThemeController {
    pub(crate) fn from_internal(themes_dir: PathBuf) -> Self {
        Self { themes_dir }
    }

    pub(crate) fn initial_snapshot(
        &self,
        theme_name: Option<&str>,
        selected: Option<&ThemeResource>,
    ) -> CodingAgentThemeSnapshot {
        selected
            .and_then(|resource| CodingAgentThemeSnapshot::from_resource(resource).ok())
            .unwrap_or_else(|| match theme_name {
                Some("light") => CodingAgentThemeSnapshot::light(),
                _ => CodingAgentThemeSnapshot::dark(),
            })
    }

    /// Resolve a settings-selected theme without exposing its source path or
    /// raw document. Invalid or transiently unavailable custom themes fail
    /// closed and leave the adapter's last good snapshot unchanged.
    pub fn select(
        &self,
        name: impl AsRef<str>,
    ) -> Result<CodingAgentThemeSnapshot, CodingAgentPublicError> {
        let name = name.as_ref();
        match name {
            "dark" => Ok(CodingAgentThemeSnapshot::dark()),
            "light" => Ok(CodingAgentThemeSnapshot::light()),
            custom => {
                let path = self.themes_dir.join(format!("{custom}.json"));
                let content = crate::platform::io::bounded::read_text(
                    &path,
                    crate::limits::MAX_THEME_FILE_BYTES,
                )
                .map_err(|error| theme_error(format!("theme could not be read: {error}")))?;
                let theme = serde_json::from_str(&content)
                    .map_err(|error| theme_error(format!("theme is invalid: {error}")))?;
                CodingAgentThemeSnapshot::from_resource(&ThemeResource {
                    name: custom.to_string(),
                    path,
                    theme,
                })
            }
        }
    }

    /// Start a bounded watcher for one active custom theme. Built-in themes
    /// produce an idle receiver so adapters can use one event-loop shape.
    pub fn watch(
        &self,
        name: impl Into<String>,
        debounce: Duration,
    ) -> Result<(CodingAgentThemeWatcher, CodingAgentThemeReloadReceiver), CodingAgentPublicError>
    {
        let (watcher, receiver) =
            ThemeWatcher::start(self.themes_dir.clone(), name.into(), debounce)
                .map_err(|error| theme_error(format!("theme watcher could not start: {error}")))?;
        Ok((
            CodingAgentThemeWatcher { inner: watcher },
            CodingAgentThemeReloadReceiver { inner: receiver },
        ))
    }
}

/// Drop guard for product-owned theme filesystem monitoring.
pub struct CodingAgentThemeWatcher {
    inner: ThemeWatcher,
}

impl fmt::Debug for CodingAgentThemeWatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = &self.inner;
        formatter
            .debug_struct("CodingAgentThemeWatcher")
            .finish_non_exhaustive()
    }
}

/// Bounded receiver yielding only fully resolved theme snapshots.
pub struct CodingAgentThemeReloadReceiver {
    inner: mpsc::Receiver<ThemeReloadSignal>,
}

impl fmt::Debug for CodingAgentThemeReloadReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentThemeReloadReceiver")
            .finish_non_exhaustive()
    }
}

impl CodingAgentThemeReloadReceiver {
    pub async fn recv(&mut self) -> Option<CodingAgentThemeSnapshot> {
        while let Some(reload) = self.inner.recv().await {
            if let Ok(snapshot) = CodingAgentThemeSnapshot::from_reload(reload) {
                return Some(snapshot);
            }
        }
        None
    }
}

fn resolved_color(color: ResolvedColor) -> CodingAgentResolvedColor {
    match color {
        ResolvedColor::Default => CodingAgentResolvedColor::Default,
        ResolvedColor::Hex(red, green, blue) => CodingAgentResolvedColor::Rgb(red, green, blue),
        ResolvedColor::Ansi256(value) => CodingAgentResolvedColor::Ansi256(value),
    }
}

fn foreground_role(role: CodingAgentThemeForeground) -> ThemeColor {
    match role {
        CodingAgentThemeForeground::Accent => ThemeColor::Accent,
        CodingAgentThemeForeground::Border => ThemeColor::Border,
        CodingAgentThemeForeground::BorderAccent => ThemeColor::BorderAccent,
        CodingAgentThemeForeground::BorderMuted => ThemeColor::BorderMuted,
        CodingAgentThemeForeground::Success => ThemeColor::Success,
        CodingAgentThemeForeground::Error => ThemeColor::Error,
        CodingAgentThemeForeground::Warning => ThemeColor::Warning,
        CodingAgentThemeForeground::Muted => ThemeColor::Muted,
        CodingAgentThemeForeground::Dim => ThemeColor::Dim,
        CodingAgentThemeForeground::Text => ThemeColor::Text,
        CodingAgentThemeForeground::ThinkingText => ThemeColor::ThinkingText,
        CodingAgentThemeForeground::UserMessageText => ThemeColor::UserMessageText,
        CodingAgentThemeForeground::CustomMessageText => ThemeColor::CustomMessageText,
        CodingAgentThemeForeground::CustomMessageLabel => ThemeColor::CustomMessageLabel,
        CodingAgentThemeForeground::ToolTitle => ThemeColor::ToolTitle,
        CodingAgentThemeForeground::ToolOutput => ThemeColor::ToolOutput,
        CodingAgentThemeForeground::MdHeading => ThemeColor::MdHeading,
        CodingAgentThemeForeground::MdLink => ThemeColor::MdLink,
        CodingAgentThemeForeground::MdLinkUrl => ThemeColor::MdLinkUrl,
        CodingAgentThemeForeground::MdCode => ThemeColor::MdCode,
        CodingAgentThemeForeground::MdCodeBlock => ThemeColor::MdCodeBlock,
        CodingAgentThemeForeground::MdCodeBlockBorder => ThemeColor::MdCodeBlockBorder,
        CodingAgentThemeForeground::MdQuote => ThemeColor::MdQuote,
        CodingAgentThemeForeground::MdQuoteBorder => ThemeColor::MdQuoteBorder,
        CodingAgentThemeForeground::MdHr => ThemeColor::MdHr,
        CodingAgentThemeForeground::MdListBullet => ThemeColor::MdListBullet,
        CodingAgentThemeForeground::ToolDiffAdded => ThemeColor::ToolDiffAdded,
        CodingAgentThemeForeground::ToolDiffRemoved => ThemeColor::ToolDiffRemoved,
        CodingAgentThemeForeground::ToolDiffContext => ThemeColor::ToolDiffContext,
        CodingAgentThemeForeground::SyntaxComment => ThemeColor::SyntaxComment,
        CodingAgentThemeForeground::SyntaxKeyword => ThemeColor::SyntaxKeyword,
        CodingAgentThemeForeground::SyntaxFunction => ThemeColor::SyntaxFunction,
        CodingAgentThemeForeground::SyntaxVariable => ThemeColor::SyntaxVariable,
        CodingAgentThemeForeground::SyntaxString => ThemeColor::SyntaxString,
        CodingAgentThemeForeground::SyntaxNumber => ThemeColor::SyntaxNumber,
        CodingAgentThemeForeground::SyntaxType => ThemeColor::SyntaxType,
        CodingAgentThemeForeground::SyntaxOperator => ThemeColor::SyntaxOperator,
        CodingAgentThemeForeground::SyntaxPunctuation => ThemeColor::SyntaxPunctuation,
        CodingAgentThemeForeground::ThinkingOff => ThemeColor::ThinkingOff,
        CodingAgentThemeForeground::ThinkingMinimal => ThemeColor::ThinkingMinimal,
        CodingAgentThemeForeground::ThinkingLow => ThemeColor::ThinkingLow,
        CodingAgentThemeForeground::ThinkingMedium => ThemeColor::ThinkingMedium,
        CodingAgentThemeForeground::ThinkingHigh => ThemeColor::ThinkingHigh,
        CodingAgentThemeForeground::ThinkingXhigh => ThemeColor::ThinkingXhigh,
        CodingAgentThemeForeground::BashMode => ThemeColor::BashMode,
    }
}

fn background_role(role: CodingAgentThemeBackground) -> ThemeBg {
    match role {
        CodingAgentThemeBackground::Selected => ThemeBg::SelectedBg,
        CodingAgentThemeBackground::UserMessage => ThemeBg::UserMessageBg,
        CodingAgentThemeBackground::CustomMessage => ThemeBg::CustomMessageBg,
        CodingAgentThemeBackground::ToolPending => ThemeBg::ToolPendingBg,
        CodingAgentThemeBackground::ToolSuccess => ThemeBg::ToolSuccessBg,
        CodingAgentThemeBackground::ToolError => ThemeBg::ToolErrorBg,
    }
}

fn theme_error(message: String) -> CodingAgentPublicError {
    CodingAgentPublicError::from(CodingSessionError::Resource { message })
}
