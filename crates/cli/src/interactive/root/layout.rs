use super::*;

pub(super) fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!(
            "{}m{:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

pub(super) fn short_id(value: &str) -> String {
    const MAX: usize = 10;
    let mut characters = value.chars();
    let short = characters.by_ref().take(MAX).collect::<String>();
    if characters.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

pub(super) fn abbreviate_path(path: &str, max_characters: usize) -> String {
    let characters = path.chars().collect::<Vec<_>>();
    if characters.len() <= max_characters {
        return path.to_owned();
    }
    let keep = max_characters.saturating_sub(1);
    format!(
        "…{}",
        characters[characters.len().saturating_sub(keep)..]
            .iter()
            .collect::<String>()
    )
}

pub(super) fn nonempty_join(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.into()
    } else {
        values.join(", ")
    }
}

pub(super) fn format_token_total(count: u64) -> String {
    u32::try_from(count).map_or_else(
        |_| format!("{:.1}B", count as f64 / 1_000_000_000.0),
        format_tokens,
    )
}

pub(super) fn context_percentage(tokens: u32, window: u32) -> u64 {
    debug_assert!(
        window > 0,
        "zero context windows are rendered as unavailable"
    );
    (u64::from(tokens) * 100 + u64::from(window) / 2) / u64::from(window)
}

pub(super) fn context_gauge(tokens: u32, window: u32, bar_width: usize, ascii: bool) -> String {
    debug_assert!(
        window > 0,
        "zero context windows are rendered as unavailable"
    );
    let percent = context_percentage(tokens, window);
    if bar_width == 0 {
        return format!("{percent}%");
    }
    let filled = ((u64::from(tokens) * bar_width as u64 + u64::from(window) / 2)
        / u64::from(window))
    .min(bar_width as u64) as usize;
    let (filled_glyph, empty_glyph) = if ascii { ('#', '-') } else { ('█', '░') };
    format!(
        "[{}{}] {percent}%",
        filled_glyph.to_string().repeat(filled),
        empty_glyph
            .to_string()
            .repeat(bar_width.saturating_sub(filled))
    )
}

pub(super) fn transcript_viewport_bounds(
    total_rows: usize,
    height: usize,
    scroll_offset: usize,
) -> (usize, usize) {
    if height == 0 || total_rows == 0 {
        return (0, 0);
    }
    let max_offset = total_rows.saturating_sub(height);
    let offset = scroll_offset.min(max_offset);
    let end = total_rows.saturating_sub(offset);
    (end.saturating_sub(height), end)
}

pub(super) fn shell_layout_mode(width: usize) -> ShellLayoutMode {
    if width >= WIDE_LAYOUT_MIN_WIDTH {
        ShellLayoutMode::Wide
    } else if width >= MEDIUM_LAYOUT_MIN_WIDTH {
        ShellLayoutMode::Medium
    } else {
        ShellLayoutMode::Narrow
    }
}

/// The rendered column width of the modal overlay for a given role, matching
/// the overlay geometry in `transient_overlay_options` and the tui overlay
/// width resolution so modal content (including its border) is sized to the
/// visible surface instead of the full terminal.
pub(super) fn modal_overlay_width(role: TransientOverlayRole, terminal_width: usize) -> usize {
    let available = match role {
        TransientOverlayRole::ModalDialog => terminal_width.saturating_sub(4),
        _ => terminal_width,
    };
    let requested = match role {
        TransientOverlayRole::ModalDialog => 72,
        TransientOverlayRole::ContextRailDetail => 38,
        TransientOverlayRole::ContextDrawerDetail => available.saturating_mul(40) / 100,
        _ => available,
    };
    requested.min(available).max(1)
}

pub(super) fn visible_context_tabs(
    width: usize,
    active: ContextTab,
) -> Vec<(ContextTab, &'static str)> {
    if width < 26 {
        return vec![(active, active.label())];
    }
    let full_width = 10
        + ContextTab::ALL
            .iter()
            .map(|tab| tab.label().len() + usize::from(*tab == active) * 2)
            .sum::<usize>()
        + ContextTab::ALL.len().saturating_sub(1);
    ContextTab::ALL
        .into_iter()
        .map(|tab| {
            let label = if full_width <= width {
                tab.label()
            } else {
                tab.compact_label()
            };
            (tab, label)
        })
        .collect()
}

pub(super) fn panel_body(panel: Rect) -> Rect {
    Rect::new(
        panel.x,
        panel.y.saturating_add(usize::from(panel.height > 0)),
        panel.width,
        panel.height.saturating_sub(1),
    )
}

pub(super) fn tail_lines(lines: &[String], height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    lines[lines.len().saturating_sub(height)..].to_vec()
}

pub(super) fn build_settings_list(
    settings: CodingAgentSettingsSnapshot,
    theme: &TuiTheme,
    keybindings: KeybindingsManager,
) -> SettingsList {
    SettingsList::with_options(
        vec![
            SettingItem::new("theme", "Theme", theme.name.clone())
                .values(["dark", "light"])
                .description("Change the active interface theme"),
            SettingItem::new(
                "auto_compaction",
                "Auto compact",
                if settings.runtime.auto_compaction {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Automatically compact context before it exceeds the model window"),
            SettingItem::new(
                "steering_mode",
                "Steering mode",
                settings.runtime.steering_mode.as_str(),
            )
            .values(["one-at-a-time", "all"])
            .description("Enter while streaming queues steering messages ('one-at-a-time' delivers one at a time)"),
            SettingItem::new(
                "follow_up_mode",
                "Follow-up mode",
                settings.runtime.follow_up_mode.as_str(),
            )
            .values(["one-at-a-time", "all"])
            .description("Queue follow-up messages until agent stops"),
            SettingItem::new(
                "show_progress",
                "Terminal progress",
                if settings.presentation.show_progress {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Show progress indicators in terminal tab bar"),
            SettingItem::new(
                "auto_resize_images",
                "Auto-resize images",
                if settings.runtime.auto_resize_images {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Resize large images to 2000\u{d7}2000 max for better model compatibility"),
            SettingItem::new(
                "block_images",
                "Block images",
                if settings.runtime.block_images {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Prevent images from being sent to LLM providers"),
            SettingItem::new(
                "enable_skill_commands",
                "Skill commands",
                if settings.runtime.enable_skill_commands {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Register skills as /skill:name commands"),
            SettingItem::new(
                "hide_thinking_block",
                "Hide thinking",
                if settings.presentation.hide_thinking_block {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Hide thinking blocks in assistant responses"),
            SettingItem::new(
                "quiet_startup",
                "Quiet startup",
                if settings.presentation.quiet_startup {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Disable verbose printing at startup"),
            SettingItem::new(
                "clear_on_shrink",
                "Clear on shrink",
                if settings.presentation.clear_on_shrink {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Clear empty rows when content shrinks (may cause flicker)"),
            SettingItem::new(
                "double_escape_action",
                "Double-escape action",
                settings.presentation.double_escape_action.as_str(),
            )
            .values(["tree", "fork", "none"])
            .description("Action when pressing Escape twice with empty editor"),
            SettingItem::new(
                "default_thinking_level",
                "Thinking level",
                settings
                    .runtime
                    .default_thinking_level
                    .unwrap_or_default()
                    .to_string(),
            )
            .values(["off", "minimal", "low", "medium", "high", "xhigh"])
            .description(
                "Default reasoning depth; DeepSeek maps minimal/low/medium/high to high and xhigh to max",
            ),
            SettingItem::new(
                "http_idle_timeout",
                "HTTP idle timeout",
                format_http_idle_timeout_ms(settings.runtime.http_idle_timeout_ms),
            )
            .values(HTTP_IDLE_TIMEOUT_CHOICES.map(|(label, _)| label))
            .description("Maximum idle gap while waiting for HTTP provider response data"),
        ],
        16,
        keybindings,
        SettingsListOptions {
            enable_search: false,
        },
    )
}

pub(super) fn tool_authorization_risk_label(risk: ToolAuthorizationRisk) -> &'static str {
    match risk {
        ToolAuthorizationRisk::ExternalRead => "external read",
        ToolAuthorizationRisk::FilesystemMutation => "filesystem mutation",
        ToolAuthorizationRisk::ShellExecution => "shell execution",
        ToolAuthorizationRisk::DeclaredSideEffect => "declared side effect",
        ToolAuthorizationRisk::Unknown => "unknown",
    }
}

pub(super) fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|(_, value)| *value == timeout_ms)
        .map(|(label, _)| (*label).to_string())
        .unwrap_or_else(|| format!("{} sec", timeout_ms as f64 / 1000.0))
}
