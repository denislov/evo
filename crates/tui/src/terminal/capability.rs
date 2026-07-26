#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

pub fn detect_terminal_capabilities_from_env<I, K, V, F>(
    env: I,
    tmux_forwards_hyperlinks: F,
) -> TerminalCapabilities
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
    F: Fn() -> bool,
{
    let mut term_program = String::new();
    let mut terminal_emulator = String::new();
    let mut term = String::new();
    let mut color_term = String::new();
    let mut tmux = false;
    let mut kitty = false;
    let mut ghostty = false;
    let mut wezterm = false;
    let mut iterm = false;
    let mut windows_terminal = false;

    for (key, value) in env {
        let key = key.as_ref();
        let value = value.as_ref().to_lowercase();
        match key {
            "TERM_PROGRAM" => term_program = value,
            "TERMINAL_EMULATOR" => terminal_emulator = value,
            "TERM" => term = value,
            "COLORTERM" => color_term = value,
            "TMUX" => tmux = true,
            "KITTY_WINDOW_ID" => kitty = true,
            "GHOSTTY_RESOURCES_DIR" => ghostty = true,
            "WEZTERM_PANE" => wezterm = true,
            "ITERM_SESSION_ID" => iterm = true,
            "WT_SESSION" => windows_terminal = true,
            _ => {}
        }
    }

    let has_true_color_hint = matches!(color_term.as_str(), "truecolor" | "24bit");
    if tmux || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: tmux_forwards_hyperlinks(),
        };
    }
    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    if kitty || term_program == "kitty" {
        return terminal_caps(Some(ImageProtocol::Kitty), true, true);
    }
    if ghostty || term_program == "ghostty" || term.contains("ghostty") {
        return terminal_caps(Some(ImageProtocol::Kitty), true, true);
    }
    if wezterm || term_program == "wezterm" {
        return terminal_caps(Some(ImageProtocol::Kitty), true, true);
    }
    if iterm || term_program == "iterm.app" {
        return terminal_caps(Some(ImageProtocol::ITerm2), true, true);
    }
    if windows_terminal || term_program == "vscode" || term_program == "alacritty" {
        return terminal_caps(None, true, true);
    }
    if terminal_emulator == "jetbrains-jediterm" {
        return terminal_caps(None, true, false);
    }

    terminal_caps(None, has_true_color_hint, false)
}

fn terminal_caps(
    images: Option<ImageProtocol>,
    true_color: bool,
    hyperlinks: bool,
) -> TerminalCapabilities {
    TerminalCapabilities {
        images,
        true_color,
        hyperlinks,
    }
}
