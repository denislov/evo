use std::io::{self, Write, stdout};
use std::time::Duration;

use crossterm::{
    cursor, execute,
    terminal::{self, Clear, ClearType},
};

use crate::input::set_kitty_protocol_active;

const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0;\x07";
const MOUSE_CAPTURE_ENABLE_SEQUENCE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_CAPTURE_DISABLE_SEQUENCE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

fn mouse_capture_enable_sequence(mode: TerminalMode) -> Option<&'static str> {
    (mode == TerminalMode::Fullscreen).then_some(MOUSE_CAPTURE_ENABLE_SEQUENCE)
}

#[cfg(windows)]
mod win32 {
    #![allow(
        non_snake_case,
        reason = "Windows console API names follow the platform ABI"
    )]

    pub type HANDLE = *mut std::ffi::c_void;
    pub const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6u32; // -10
    pub const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    unsafe extern "system" {
        pub fn GetStdHandle(nStdHandle: u32) -> HANDLE;
        pub fn GetConsoleMode(hConsoleHandle: HANDLE, lpMode: *mut u32) -> i32;
        pub fn SetConsoleMode(hConsoleHandle: HANDLE, dwMode: u32) -> i32;
    }
}

const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u\x1b[c";
const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: u64 = 150;

/// Apple Terminal Shift+Enter sequence (sent when Shift is pressed with Enter).
const APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";

/// Detect whether the current session is Apple Terminal.
pub fn is_apple_terminal_session() -> bool {
    std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal")
}

/// Normalize Apple Terminal input.
///
/// Apple Terminal does not send unique escape sequences for Shift+Enter
/// (both Enter and Shift+Enter send `\r`). This function rewrites `\r`
/// to `\x1b[13;2u` when the Shift key is held.
///
/// The `shift_pressed` parameter should come from a platform-specific
/// native modifier detector (e.g. `CGEventSourceFlagsState` on macOS).
/// When no native detector is available, pass `false` — Shift+Enter
/// will fall through as plain Enter, which is the safest behaviour.
pub fn normalize_apple_terminal_input(data: &str, shift_pressed: bool) -> String {
    if is_apple_terminal_session() && data == "\r" && shift_pressed {
        APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE.to_string()
    } else {
        data.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalMode {
    #[default]
    Inline,
    Fullscreen,
}

pub trait Terminal {
    fn size(&self) -> TerminalSize;
    fn write(&mut self, data: &str) -> std::io::Result<()>;
    fn move_by(&mut self, rows: i16) -> std::io::Result<()>;
    fn move_to_column(&mut self, column: usize) -> std::io::Result<()>;
    fn hide_cursor(&mut self) -> std::io::Result<()>;
    fn show_cursor(&mut self) -> std::io::Result<()>;
    fn clear_line(&mut self) -> std::io::Result<()>;
    fn clear_from_cursor(&mut self) -> std::io::Result<()>;
    fn clear_screen(&mut self) -> std::io::Result<()>;
    fn flush(&mut self) -> std::io::Result<()>;

    fn start(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn start_mode(&mut self, _mode: TerminalMode) -> std::io::Result<()> {
        self.start()
    }

    fn stop(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn drain_input(&mut self, _max: Duration, _idle: Duration) -> std::io::Result<()> {
        Ok(())
    }

    fn set_title(&mut self, _title: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn set_progress(&mut self, _active: bool) -> std::io::Result<()> {
        Ok(())
    }

    fn kitty_protocol_active(&self) -> bool {
        false
    }
}

/// Result of feeding data through the Kitty protocol negotiation state machine.
#[derive(Debug, Clone)]
pub enum NegotiationResult {
    /// Still waiting for the terminal's protocol response.
    /// The caller should feed more stdin data.
    Negotiating,
    /// Negotiation is complete. `forward` contains any non-negotiation
    /// sequences that arrived alongside the response and should be
    /// forwarded to the input handler.
    Done { forward: Vec<String> },
}

pub struct ProcessTerminal {
    raw_mode_enabled_by_us: bool,
    #[cfg(windows)]
    windows_stdin_original_mode: Option<u32>,
    kitty_protocol_active: bool,
    modify_other_keys_active: bool,
    keyboard_protocol_pushed: bool,
    alternate_screen_active: bool,
    mouse_capture_active: bool,
    negotiation_buffer: String,
    negotiation_done: bool,
    negotiation_flush_deadline: Option<std::time::Instant>,
    progress_active: bool,
}

impl ProcessTerminal {
    pub fn new() -> Self {
        Self {
            raw_mode_enabled_by_us: false,
            #[cfg(windows)]
            windows_stdin_original_mode: None,
            kitty_protocol_active: false,
            modify_other_keys_active: false,
            keyboard_protocol_pushed: false,
            alternate_screen_active: false,
            mouse_capture_active: false,
            negotiation_buffer: String::new(),
            negotiation_done: false,
            negotiation_flush_deadline: None,
            progress_active: false,
        }
    }

    /// On Windows, enable `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200) on the
    /// stdin console handle so that the terminal sends VT escape sequences
    /// for modified keys (e.g. `\x1b[Z` for Shift+Tab).
    ///
    /// Must be called AFTER raw mode is enabled, because
    /// [`crossterm::terminal::enable_raw_mode`] resets the console mode
    /// flags via `SetConsoleMode`, which would clear this flag if set
    /// before.
    #[cfg(windows)]
    pub fn enable_windows_vt_input(&mut self) {
        use win32::*;
        let invalid = (-1isize) as HANDLE;
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == invalid {
                return;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_INPUT == 0 {
                mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
                SetConsoleMode(handle, mode);
            }
        }
    }

    #[cfg(not(windows))]
    pub fn enable_windows_vt_input(&mut self) {}

    #[cfg(windows)]
    fn save_windows_stdin_mode(&mut self) {
        if self.windows_stdin_original_mode.is_some() {
            return;
        }

        use win32::*;
        let invalid = (-1isize) as HANDLE;
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == invalid {
                return;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                self.windows_stdin_original_mode = Some(mode);
            }
        }
    }

    #[cfg(windows)]
    fn restore_windows_stdin_mode(&mut self) -> io::Result<bool> {
        let Some(original_mode) = self.windows_stdin_original_mode else {
            return Ok(false);
        };

        use win32::*;
        let invalid = (-1isize) as HANDLE;
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == invalid {
                return Ok(false);
            }
            if SetConsoleMode(handle, original_mode) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        self.windows_stdin_original_mode = None;
        Ok(true)
    }

    #[cfg(not(windows))]
    fn restore_windows_stdin_mode(&mut self) -> std::io::Result<bool> {
        Ok(false)
    }

    /// Feed raw stdin data through the Kitty protocol negotiation state
    /// machine. Call this after [`Terminal::start`] until it returns
    /// [`NegotiationResult::Done`], then switch to feeding data through
    /// [`crate::input::StdinBuffer`].
    ///
    /// The negotiation intercepts `\x1b[?Nu` (Kitty flags response) and
    /// `\x1b[?...c` (Device Attributes).  When Kitty responds with non-zero
    /// flags the protocol is enabled; when DA arrives first (terminals that
    /// do not speak Kitty) modifyOtherKeys is enabled as a fallback.
    ///
    /// Kitty flags are always processed even after DA has been received,
    /// so that late-arriving Kitty responses can still enable the protocol.
    pub fn negotiate(&mut self, data: &str) -> NegotiationResult {
        self.negotiation_buffer.push_str(data);

        // Always try to extract a negotiation response first, even if
        // we've already seen DA — late Kitty flags should still be honoured.
        if let Some(result) = self.try_extract_negotiation_response() {
            return result;
        }

        // If negotiation is already done, forward everything.
        if self.negotiation_done {
            let remaining = std::mem::take(&mut self.negotiation_buffer);
            return NegotiationResult::Done {
                forward: if remaining.is_empty() {
                    Vec::new()
                } else {
                    vec![remaining]
                },
            };
        }

        // If the buffer looks like a partial negotiation prefix, keep waiting.
        if is_negotiation_prefix(&self.negotiation_buffer) {
            self.negotiation_flush_deadline = Some(
                std::time::Instant::now()
                    + Duration::from_millis(KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS),
            );
            return NegotiationResult::Negotiating;
        }

        // Buffer doesn't look like a negotiation sequence at all — flush it
        // as regular input and mark negotiation done (we missed the response
        // or the terminal doesn't support the query).
        self.finish_negotiation()
    }

    /// Call periodically (e.g. in the main event loop) to flush a stalled
    /// negotiation buffer after the fragment timeout elapses.
    pub fn tick_negotiation(&mut self) -> NegotiationResult {
        if self.negotiation_done {
            return NegotiationResult::Done {
                forward: Vec::new(),
            };
        }

        let Some(deadline) = self.negotiation_flush_deadline else {
            return NegotiationResult::Negotiating;
        };

        if std::time::Instant::now() >= deadline {
            self.finish_negotiation()
        } else {
            NegotiationResult::Negotiating
        }
    }

    /// True once negotiation has completed (or timed out).
    pub fn negotiation_done(&self) -> bool {
        self.negotiation_done
    }

    fn try_extract_negotiation_response(&mut self) -> Option<NegotiationResult> {
        // Check for Kitty flags response: \x1b[?Nu
        if let Some(flags) = parse_kitty_flags_response(&self.negotiation_buffer) {
            self.clear_negotiation_buffer();
            if flags != 0 {
                let _ = self.disable_modify_other_keys();
                if !self.kitty_protocol_active {
                    self.kitty_protocol_active = true;
                    set_kitty_protocol_active(true);
                }
            } else {
                self.enable_modify_other_keys();
            }
            // Kitty flags response is definitive — negotiation is done.
            self.negotiation_done = true;
            return Some(NegotiationResult::Done {
                forward: Vec::new(),
            });
        }

        // Check for Device Attributes response: \x1b[?...c
        // DA is a sentinel: terminals that don't know Kitty protocol respond
        // with DA first. We enable modifyOtherKeys but keep waiting — Kitty
        // flags may still arrive afterwards.
        if is_device_attributes_response(&self.negotiation_buffer) {
            self.clear_negotiation_buffer();
            if !self.kitty_protocol_active {
                self.enable_modify_other_keys();
            }
            // Don't mark negotiation done yet — Kitty flags may follow.
            return Some(NegotiationResult::Negotiating);
        }

        None
    }

    fn finish_negotiation(&mut self) -> NegotiationResult {
        let remaining = std::mem::take(&mut self.negotiation_buffer);
        self.negotiation_done = true;
        self.negotiation_flush_deadline = None;

        if remaining.is_empty() {
            NegotiationResult::Done {
                forward: Vec::new(),
            }
        } else {
            NegotiationResult::Done {
                forward: vec![remaining],
            }
        }
    }

    fn clear_negotiation_buffer(&mut self) {
        self.negotiation_buffer.clear();
        self.negotiation_flush_deadline = None;
    }

    fn enable_modify_other_keys(&mut self) {
        if self.kitty_protocol_active || self.modify_other_keys_active {
            return;
        }
        let _ = self.write("\x1b[>4;2m");
        self.modify_other_keys_active = true;
    }

    fn disable_modify_other_keys(&mut self) -> io::Result<()> {
        if !self.modify_other_keys_active {
            return Ok(());
        }
        let result = self.write("\x1b[>4;0m");
        self.modify_other_keys_active = false;
        result
    }

    fn disable_mouse_capture(&mut self) -> io::Result<()> {
        if !self.mouse_capture_active {
            return Ok(());
        }
        let result = self.write(MOUSE_CAPTURE_DISABLE_SEQUENCE);
        self.mouse_capture_active = false;
        result
    }
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

fn record_cleanup_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(error) = result
        && first_error.is_none()
    {
        *first_error = Some(error);
    }
}

impl Terminal for ProcessTerminal {
    fn size(&self) -> TerminalSize {
        let (columns, rows) = terminal::size().unwrap_or((80, 24));
        TerminalSize {
            columns: columns as usize,
            rows: rows as usize,
        }
    }

    fn write(&mut self, data: &str) -> std::io::Result<()> {
        stdout().write_all(data.as_bytes())
    }

    fn move_by(&mut self, rows: i16) -> std::io::Result<()> {
        let mut out = stdout();
        if rows < 0 {
            execute!(out, cursor::MoveUp((-rows) as u16))?;
        } else if rows > 0 {
            execute!(out, cursor::MoveDown(rows as u16))?;
        }
        Ok(())
    }

    fn move_to_column(&mut self, column: usize) -> std::io::Result<()> {
        self.write(&format!("\x1b[{}G", column.saturating_add(1)))
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        execute!(stdout(), cursor::Hide)
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        execute!(stdout(), cursor::Show)
    }

    fn clear_line(&mut self) -> std::io::Result<()> {
        execute!(stdout(), Clear(ClearType::CurrentLine))
    }

    fn clear_from_cursor(&mut self) -> std::io::Result<()> {
        execute!(stdout(), Clear(ClearType::FromCursorDown))
    }

    fn clear_screen(&mut self) -> std::io::Result<()> {
        execute!(stdout(), Clear(ClearType::All), cursor::MoveTo(0, 0))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        stdout().flush()
    }

    fn start(&mut self) -> std::io::Result<()> {
        self.start_mode(TerminalMode::Inline)
    }

    fn start_mode(&mut self, mode: TerminalMode) -> std::io::Result<()> {
        #[cfg(windows)]
        self.save_windows_stdin_mode();

        let already_raw = terminal::is_raw_mode_enabled().unwrap_or(false);
        if !already_raw {
            terminal::enable_raw_mode()?;
            self.raw_mode_enabled_by_us = true;
        }
        // Enable ENABLE_VIRTUAL_TERMINAL_INPUT on Windows (run AFTER
        // enable_raw_mode since that resets console mode flags).
        self.enable_windows_vt_input();
        if mode == TerminalMode::Fullscreen {
            self.write("\x1b[?1049h")?;
            self.alternate_screen_active = true;
        }
        if let Some(sequence) = mouse_capture_enable_sequence(mode) {
            self.mouse_capture_active = true;
            self.write(sequence)?;
        }
        // Enable bracketed paste
        self.write("\x1b[?2004h")?;
        // Query Kitty keyboard protocol (flags 7: disambiguate + event types + alternate keys)
        // The trailing DA query (\x1b[c) acts as a sentinel: terminals that don't know
        // Kitty protocol will respond with DA first, triggering modifyOtherKeys fallback.
        self.write(KITTY_KEYBOARD_PROTOCOL_QUERY)?;
        self.keyboard_protocol_pushed = true;
        self.negotiation_buffer.clear();
        self.negotiation_done = false;
        self.negotiation_flush_deadline = None;
        self.hide_cursor()?;
        self.flush()
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let mut first_error = None;
        record_cleanup_error(&mut first_error, self.write("\x1b[?2026l"));
        record_cleanup_error(&mut first_error, self.set_progress(false));
        record_cleanup_error(&mut first_error, self.write("\x1b[?2004l"));
        record_cleanup_error(&mut first_error, self.disable_mouse_capture());
        let should_disable = self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.negotiation_buffer.clear();
        self.negotiation_done = true;

        if should_disable {
            record_cleanup_error(&mut first_error, self.write("\x1b[<u"));
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        record_cleanup_error(&mut first_error, self.disable_modify_other_keys());
        record_cleanup_error(&mut first_error, self.show_cursor());
        if self.alternate_screen_active {
            record_cleanup_error(&mut first_error, self.write("\x1b[?1049l"));
            self.alternate_screen_active = false;
        }
        record_cleanup_error(&mut first_error, self.flush());
        let restored_windows_mode = match self.restore_windows_stdin_mode() {
            Ok(restored) => restored,
            Err(error) => {
                record_cleanup_error(&mut first_error, Err(error));
                false
            }
        };
        if self.raw_mode_enabled_by_us && !restored_windows_mode {
            record_cleanup_error(&mut first_error, terminal::disable_raw_mode());
        }
        self.raw_mode_enabled_by_us = false;
        first_error.map_or(Ok(()), Err)
    }

    /// Drain stdin before exiting to prevent Kitty key release events from
    /// leaking to the parent shell.  Disables Kitty protocol first, then
    /// waits up to `max` for input to settle (exits early after `idle` of
    /// silence).
    fn drain_input(&mut self, max: Duration, idle: Duration) -> std::io::Result<()> {
        let should_disable = self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.negotiation_buffer.clear();
        self.negotiation_done = true;

        if should_disable {
            // Disable Kitty keyboard protocol so late key releases don't
            // generate new escape sequences.
            self.write("\x1b[<u")?;
            self.flush()?;
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        let _ = self.disable_modify_other_keys();
        let _ = self.disable_mouse_capture();
        self.flush()?;

        // Simple drain: sleep for `idle` to let in-flight data arrive and be
        // discarded by the OS terminal buffer.  A full non-blocking drain
        // would require platform-specific I/O which is out of scope for now.
        let wait = idle.min(max);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        Ok(())
    }

    fn set_title(&mut self, title: &str) -> std::io::Result<()> {
        self.write(&format!("\x1b]0;{title}\x07"))
    }

    fn set_progress(&mut self, active: bool) -> std::io::Result<()> {
        if active == self.progress_active {
            return Ok(());
        }

        if active {
            self.write(TERMINAL_PROGRESS_ACTIVE_SEQUENCE)?;
            self.flush()?;
            self.progress_active = true;
        } else {
            self.progress_active = false;
            self.write(TERMINAL_PROGRESS_CLEAR_SEQUENCE)?;
            self.flush()?;
        }
        Ok(())
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }
}

// ── Negotiation sequence parsing ──────────────────────────────────────────

fn parse_kitty_flags_response(sequence: &str) -> Option<u8> {
    let body = sequence.strip_prefix("\x1b[?")?.strip_suffix('u')?;
    body.parse().ok()
}

fn is_device_attributes_response(sequence: &str) -> bool {
    sequence.starts_with("\x1b[?") && sequence.ends_with('c') && sequence.len() > 4
}

fn is_negotiation_prefix(sequence: &str) -> bool {
    sequence == "\x1b["
        || (sequence.starts_with("\x1b[?") && !sequence.contains('c') && !sequence.ends_with('u'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TERMINAL_DRAIN_INPUT_MAX: Duration = Duration::from_millis(100);
    const TERMINAL_DRAIN_INPUT_IDLE: Duration = Duration::from_millis(10);

    #[test]
    fn kitty_flags_response_enables_protocol() {
        let mut term = ProcessTerminal::new();
        // Simulate: start() sends query, terminal responds with flags=7
        term.keyboard_protocol_pushed = true;
        let result = term.negotiate("\x1b[?7u");
        assert!(matches!(result, NegotiationResult::Done { .. }));
        assert!(term.kitty_protocol_active);
    }

    #[test]
    fn kitty_zero_flags_falls_back_to_modify_other_keys() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        let result = term.negotiate("\x1b[?0u");
        assert!(matches!(result, NegotiationResult::Done { .. }));
        assert!(!term.kitty_protocol_active);
        assert!(term.modify_other_keys_active);
    }

    #[test]
    fn device_attributes_before_kitty_triggers_modify_other_keys() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        // DA arrives first → modifyOtherKeys enabled, but negotiation continues
        let result = term.negotiate("\x1b[?1;2c");
        assert!(matches!(result, NegotiationResult::Negotiating));
        assert!(!term.kitty_protocol_active);
        assert!(term.modify_other_keys_active);
        assert!(!term.negotiation_done);
    }

    #[test]
    fn kitty_response_after_da_does_not_reenable_modify_other_keys() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        // DA arrives first → modifyOtherKeys enabled
        let _ = term.negotiate("\x1b[?1;2c");
        assert!(term.modify_other_keys_active);
        // Then Kitty flags arrive → disable modifyOtherKeys, enable Kitty
        let result = term.negotiate("\x1b[?7u");
        assert!(matches!(result, NegotiationResult::Done { .. }));
        assert!(term.kitty_protocol_active);
        assert!(!term.modify_other_keys_active);
    }

    #[test]
    fn partial_negotiation_prefix_returns_negotiating() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        let result = term.negotiate("\x1b[?");
        assert!(matches!(result, NegotiationResult::Negotiating));
        assert!(!term.negotiation_done);
    }

    #[test]
    fn non_negotiation_data_flushes_and_marks_done() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        let result = term.negotiate("hello");
        match result {
            NegotiationResult::Done { forward } => {
                assert_eq!(forward, vec!["hello".to_string()]);
            }
            _ => panic!("expected Done"),
        }
        assert!(term.negotiation_done);
    }

    #[test]
    fn stop_disables_kitty_and_modify_other_keys() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        term.kitty_protocol_active = true;
        term.modify_other_keys_active = true;
        term.alternate_screen_active = true;
        term.mouse_capture_active = true;
        // stop() writes to stdout — just verify state is cleared
        term.stop().unwrap();
        assert!(!term.kitty_protocol_active);
        assert!(!term.modify_other_keys_active);
        assert!(!term.keyboard_protocol_pushed);
        assert!(!term.alternate_screen_active);
        assert!(!term.mouse_capture_active);
    }

    #[test]
    fn mouse_capture_is_enabled_only_for_fullscreen_mode() {
        assert_eq!(
            mouse_capture_enable_sequence(TerminalMode::Fullscreen),
            Some(MOUSE_CAPTURE_ENABLE_SEQUENCE)
        );
        assert_eq!(mouse_capture_enable_sequence(TerminalMode::Inline), None);
    }

    #[test]
    fn drain_input_disables_kitty_and_sleeps() {
        let mut term = ProcessTerminal::new();
        term.keyboard_protocol_pushed = true;
        term.kitty_protocol_active = true;
        term.mouse_capture_active = true;
        term.drain_input(TERMINAL_DRAIN_INPUT_MAX, TERMINAL_DRAIN_INPUT_IDLE)
            .unwrap();
        assert!(!term.kitty_protocol_active);
        assert!(!term.keyboard_protocol_pushed);
        assert!(!term.mouse_capture_active);
    }

    #[test]
    fn progress_state_is_owned_by_the_terminal() {
        let mut term = ProcessTerminal::new();
        term.set_progress(true).unwrap();
        assert!(term.progress_active);

        // Repeating the same state is an idempotent operation owned by this
        // terminal instance; it never starts an independent writer.
        term.set_progress(true).unwrap();
        assert!(term.progress_active);

        term.set_progress(false).unwrap();
        assert!(!term.progress_active);
    }

    #[test]
    fn stop_cleans_up_progress_state() {
        let mut term = ProcessTerminal::new();
        term.set_progress(true).unwrap();
        term.stop().unwrap();
        assert!(!term.progress_active);
    }
}
