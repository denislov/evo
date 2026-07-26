use std::time::{Duration, Instant};

use super::{InputEvent, parse_key, parse_sgr_mouse};

const ESC: &str = "\x1b";
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";
const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct StdinBuffer {
    buffer: String,
    paste_buffer: String,
    in_paste: bool,
    pending_timeout: Duration,
    pending_since: Option<Instant>,
    /// Tracks the codepoint of the last emitted Kitty CSI-u printable
    /// sequence so that the release event for the same key is not
    /// emitted as a duplicate character.
    pending_kitty_printable_codepoint: Option<u32>,
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            paste_buffer: String::new(),
            in_paste: false,
            pending_timeout: DEFAULT_PENDING_TIMEOUT,
            pending_since: None,
            pending_kitty_printable_codepoint: None,
        }
    }
}

impl StdinBuffer {
    /// Create a buffer using the default 10ms idle timeout for incomplete
    /// escape sequences (matches the TypeScript `pi` reference).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a buffer that flushes incomplete escape sequences after
    /// `pending_timeout` has elapsed without further input.
    ///
    /// Use [`Duration::MAX`] to disable timeout-driven flushing (callers must
    /// then drive [`StdinBuffer::flush`] explicitly).
    pub fn with_pending_timeout(pending_timeout: Duration) -> Self {
        Self {
            pending_timeout,
            ..Self::default()
        }
    }

    /// Configure the idle timeout for incomplete escape sequences.
    pub fn set_pending_timeout(&mut self, pending_timeout: Duration) {
        self.pending_timeout = pending_timeout;
    }

    /// Returns the configured idle timeout for incomplete escape sequences.
    pub fn pending_timeout_duration(&self) -> Duration {
        self.pending_timeout
    }

    pub fn process(&mut self, data: &str) -> Vec<InputEvent> {
        self.process_at(data, Instant::now())
    }

    /// Same as [`StdinBuffer::process`], but uses the supplied instant as
    /// "now" when stamping the pending residual. Useful for deterministic
    /// tests.
    pub fn process_at(&mut self, data: &str, now: Instant) -> Vec<InputEvent> {
        self.buffer.push_str(data);
        let mut events = Vec::new();

        loop {
            if self.in_paste {
                if let Some(end_index) = self.buffer.find(BRACKETED_PASTE_END) {
                    self.paste_buffer.push_str(&self.buffer[..end_index]);
                    let remainder_start = end_index + BRACKETED_PASTE_END.len();
                    let remainder = self.buffer[remainder_start..].to_string();
                    self.buffer.clear();
                    self.buffer.push_str(&remainder);
                    self.in_paste = false;
                    self.pending_kitty_printable_codepoint = None;
                    events.push(InputEvent::Paste(std::mem::take(&mut self.paste_buffer)));
                    continue;
                }

                self.paste_buffer.push_str(&self.buffer);
                self.buffer.clear();
                break;
            }

            if self.buffer.is_empty() {
                break;
            }

            if self.buffer.starts_with(BRACKETED_PASTE_START) {
                let remainder = self.buffer[BRACKETED_PASTE_START.len()..].to_string();
                self.buffer.clear();
                self.buffer.push_str(&remainder);
                self.in_paste = true;
                continue;
            }

            let Some(sequence_len) = next_sequence_len(&self.buffer) else {
                break;
            };
            let sequence = self.buffer[..sequence_len].to_string();
            let remainder = self.buffer[sequence_len..].to_string();
            self.buffer.clear();
            self.buffer.push_str(&remainder);

            // Kitty printable dedup: when the terminal sends CSI-u for both
            // press and release of a printable key, skip the release event
            // so the character is not inserted twice.
            if let Some(event) = self.emit_sequence(&sequence) {
                events.push(event);
            }
        }

        self.refresh_pending_since(now);
        events
    }

    /// Flush any incomplete residual. Used when stdin is closing or the host
    /// otherwise wants to force-emit whatever is buffered.
    pub fn flush(&mut self) -> Vec<InputEvent> {
        let mut events = Vec::new();
        if self.in_paste {
            events.push(InputEvent::Paste(std::mem::take(&mut self.paste_buffer)));
            self.in_paste = false;
        }
        self.pending_kitty_printable_codepoint = None;
        events.extend(self.drain_pending_residual());
        events
    }

    /// If non-paste residual has been waiting longer than the configured
    /// pending timeout, emit it as if [`StdinBuffer::flush`] had been called.
    /// Otherwise returns an empty vector.
    pub fn tick(&mut self, now: Instant) -> Vec<InputEvent> {
        match self.pending_timeout_at(now) {
            Some(remaining) if remaining.is_zero() => self.drain_pending_residual(),
            _ => Vec::new(),
        }
    }

    /// Returns the remaining time before pending residual should be flushed
    /// (using the configured timeout). `Some(Duration::ZERO)` means it is
    /// already due, `None` means no residual is pending or timeouts are
    /// disabled.
    pub fn pending_timeout_at(&self, now: Instant) -> Option<Duration> {
        let started = self.pending_since?;
        if self.pending_timeout == Duration::MAX {
            return None;
        }
        let deadline = started.checked_add(self.pending_timeout)?;
        Some(deadline.saturating_duration_since(now))
    }

    /// True when there is non-paste residual currently parked in the buffer.
    pub fn has_pending_residual(&self) -> bool {
        self.pending_since.is_some()
    }

    /// Process raw bytes, converting high-byte (>127) single bytes to
    /// ESC + (byte - 128) for compatibility with legacy terminal input.
    pub fn process_bytes(&mut self, data: &[u8]) -> Vec<InputEvent> {
        if data.len() == 1 && data[0] > 127 {
            let byte = data[0] - 128;
            if let Some(ch) = char::from_u32(byte as u32) {
                return self.process(&format!("\x1b{ch}"));
            }
        }
        let s = String::from_utf8_lossy(data);
        self.process(&s)
    }

    fn emit_sequence(&mut self, sequence: &str) -> Option<InputEvent> {
        // Kitty printable dedup: when the terminal sends CSI-u for both
        // press and release of a printable key, skip the duplicate.
        if let Some(cp) = parse_unmodified_kitty_printable_codepoint(sequence) {
            if self.pending_kitty_printable_codepoint == Some(cp) {
                self.pending_kitty_printable_codepoint = None;
                return None;
            }
            self.pending_kitty_printable_codepoint = Some(cp);
        }

        Some(if let Some(mouse) = parse_sgr_mouse(sequence) {
            InputEvent::Mouse(mouse)
        } else {
            parse_key(sequence)
                .map(InputEvent::Key)
                .unwrap_or_else(|| InputEvent::Raw(sequence.to_string()))
        })
    }

    fn drain_pending_residual(&mut self) -> Vec<InputEvent> {
        self.pending_since = None;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let data = std::mem::take(&mut self.buffer);
        let event = if let Some(mouse) = parse_sgr_mouse(&data) {
            InputEvent::Mouse(mouse)
        } else {
            parse_key(&data)
                .map(InputEvent::Key)
                .unwrap_or(InputEvent::Raw(data))
        };
        vec![event]
    }

    fn refresh_pending_since(&mut self, now: Instant) {
        if !self.in_paste && !self.buffer.is_empty() {
            if self.pending_since.is_none() {
                self.pending_since = Some(now);
            }
        } else {
            self.pending_since = None;
        }
    }
}

fn next_sequence_len(buffer: &str) -> Option<usize> {
    if !buffer.starts_with(ESC) {
        return buffer.chars().next().map(char::len_utf8);
    }

    if buffer.len() == ESC.len() {
        return None;
    }

    // WezTerm fix: \x1b\x1b followed by CSI/OSC/SS3/DCS/APC means an
    // ESC press got concatenated with a Kitty CSI-u release sequence.
    // Emit only the first ESC so the CSI-u part is parsed separately.
    if buffer.starts_with("\x1b\x1b") && buffer.len() > 2 {
        let next = buffer.as_bytes()[2];
        if matches!(next, b'[' | b']' | b'O' | b'P' | b'_') {
            return Some(1);
        }
    }

    if buffer.starts_with("\x1b[") {
        return csi_sequence_len(buffer);
    }
    if buffer.starts_with("\x1b]") {
        return osc_sequence_len(buffer);
    }
    if buffer.starts_with("\x1bP") || buffer.starts_with("\x1b_") {
        return string_terminated_sequence_len(buffer);
    }
    if buffer.starts_with("\x1bO") {
        return nth_char_end(buffer, 3);
    }

    nth_char_end(buffer, 2)
}

fn csi_sequence_len(buffer: &str) -> Option<usize> {
    if buffer.len() < 3 {
        return None;
    }

    for (index, byte) in buffer.as_bytes().iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(byte) {
            return Some(index + 1);
        }
    }
    None
}

fn osc_sequence_len(buffer: &str) -> Option<usize> {
    if let Some(index) = buffer.find('\x07') {
        return Some(index + 1);
    }
    string_terminated_sequence_len(buffer)
}

fn string_terminated_sequence_len(buffer: &str) -> Option<usize> {
    buffer.find("\x1b\\").map(|index| index + 2)
}

fn nth_char_end(buffer: &str, count: usize) -> Option<usize> {
    let mut end = 0;
    for (index, ch) in buffer.char_indices().take(count) {
        end = index + ch.len_utf8();
    }
    if buffer.chars().count() >= count {
        Some(end)
    } else {
        None
    }
}

/// Extract the codepoint from a Kitty CSI-u sequence that represents a
/// plain (unmodified) printable key press.  Returns `None` for modified
/// keys or non-printable codepoints.
fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    // Match \x1b[<codepoint>u or \x1b[<codepoint>;1u (no modifiers)
    let body = sequence.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let codepoint: u32 = body.split(':').next()?.parse().ok()?;
    // Only printable codepoints (space and above)
    if codepoint >= 32 {
        Some(codepoint)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

    const STDIN_BUFFER_PENDING_TIMEOUT: Duration = Duration::from_millis(10);
    const STDIN_BUFFER_PARTIAL_WAIT: Duration = Duration::from_millis(5);
    const STDIN_BUFFER_FOLLOWUP_OFFSET: Duration = Duration::from_millis(2);
    const STDIN_BUFFER_ELAPSED_BEFORE_TIMEOUT: Duration = Duration::from_millis(3);
    const STDIN_BUFFER_REMAINING_TIMEOUT: Duration = Duration::from_millis(7);

    fn stdin_buffer_clock_anchor() -> Instant {
        Instant::now()
    }

    #[test]
    fn lone_escape_is_held_until_timeout_elapses() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        let immediate = buffer.process_at("\x1b", start);
        assert!(
            immediate.is_empty(),
            "lone escape must not emit until timeout elapses"
        );
        assert!(buffer.has_pending_residual());

        let still_pending = buffer.tick(start + STDIN_BUFFER_PARTIAL_WAIT);
        assert!(still_pending.is_empty());
        assert!(buffer.has_pending_residual());

        let flushed = buffer.tick(start + STDIN_BUFFER_PENDING_TIMEOUT);
        assert_eq!(flushed.len(), 1);
        match &flushed[0] {
            InputEvent::Key(event) => {
                assert_eq!(event.key, Key::Escape);
                assert_eq!(event.modifiers, KeyModifiers::empty());
                assert_eq!(event.kind, KeyEventKind::Press);
            }
            other => panic!("expected key event, got {other:?}"),
        }
        assert!(!buffer.has_pending_residual());
    }

    #[test]
    fn sgr_mouse_sequence_emits_a_typed_mouse_event() {
        let mut buffer = StdinBuffer::new();
        let events = buffer.process("\x1b[<0;9;6M");

        assert!(matches!(
            events.as_slice(),
            [InputEvent::Mouse(event)]
                if event.kind == MouseEventKind::Down(MouseButton::Left)
                    && event.column == 8
                    && event.row == 5
        ));
    }

    #[test]
    fn split_sgr_mouse_sequence_waits_for_the_final_byte() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);

        assert!(buffer.process_at("\x1b[<0;9", start).is_empty());
        let events = buffer.process_at(";6M", start + STDIN_BUFFER_FOLLOWUP_OFFSET);

        assert!(matches!(
            events.as_slice(),
            [InputEvent::Mouse(event)]
                if event.kind == MouseEventKind::Down(MouseButton::Left)
                    && event.column == 8
                    && event.row == 5
        ));
    }

    #[test]
    fn split_csi_sequence_is_combined_when_followup_arrives_before_timeout() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        let first = buffer.process_at("\x1b", start);
        assert!(first.is_empty());
        assert!(buffer.has_pending_residual());

        let second = buffer.process_at("[A", start + STDIN_BUFFER_FOLLOWUP_OFFSET);
        assert_eq!(second.len(), 1);
        match &second[0] {
            InputEvent::Key(event) => assert_eq!(event.key, Key::Up),
            other => panic!("expected up arrow key, got {other:?}"),
        }
        assert!(!buffer.has_pending_residual());
    }

    #[test]
    fn pending_timeout_at_reports_remaining_time() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        assert!(buffer.pending_timeout_at(start).is_none());
        buffer.process_at("\x1b", start);

        let remaining = buffer
            .pending_timeout_at(start + STDIN_BUFFER_ELAPSED_BEFORE_TIMEOUT)
            .expect("residual is pending");
        assert_eq!(remaining, STDIN_BUFFER_REMAINING_TIMEOUT);

        let due = buffer
            .pending_timeout_at(start + STDIN_BUFFER_PENDING_TIMEOUT)
            .expect("residual is pending");
        assert_eq!(due, Duration::ZERO);
    }

    #[test]
    fn flush_drains_residual_immediately() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        buffer.process_at("\x1b", start);
        let flushed = buffer.flush();
        assert_eq!(flushed.len(), 1);
        match &flushed[0] {
            InputEvent::Key(event) => assert_eq!(event.key, Key::Escape),
            other => panic!("expected escape, got {other:?}"),
        }
        assert!(!buffer.has_pending_residual());
    }

    #[test]
    fn paste_in_progress_does_not_set_pending_residual() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        let events = buffer.process_at("\x1b[200~hello", start);
        assert!(events.is_empty());
        assert!(
            !buffer.has_pending_residual(),
            "paste content should not be subject to the escape-flush timeout"
        );
    }

    #[test]
    fn complete_inputs_clear_pending_state() {
        let start = stdin_buffer_clock_anchor();
        let mut buffer = StdinBuffer::with_pending_timeout(STDIN_BUFFER_PENDING_TIMEOUT);
        let events = buffer.process_at("abc", start);
        assert_eq!(events.len(), 3);
        assert!(!buffer.has_pending_residual());
        assert!(buffer.pending_timeout_at(start).is_none());
    }

    #[test]
    fn wezterm_double_escape_split_emits_escape_then_csi_u() {
        let mut buffer = StdinBuffer::new();
        // \x1b\x1b[97u = ESC press + 'a' CSI-u release (WezTerm concatenation)
        let events = buffer.process("\x1b\x1b[97u");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            InputEvent::Key(KeyEvent {
                key: Key::Escape,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
        match &events[1] {
            InputEvent::Key(ke) => {
                assert_eq!(ke.key, Key::Char("a".to_string()));
            }
            _ => panic!("expected key event for 'a'"),
        }
    }

    #[test]
    fn kitty_printable_dedup_skips_duplicate_release() {
        let mut buffer = StdinBuffer::new();
        // Simulate Kitty protocol: press sends \x1b[97u, release sends \x1b[97u again
        let events = buffer.process("\x1b[97u\x1b[97u");
        // First is the press, second should be suppressed by dedup
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Key(ke) => {
                assert_eq!(ke.key, Key::Char("a".to_string()));
            }
            _ => panic!("expected key event for 'a'"),
        }
    }

    #[test]
    fn high_byte_conversion_becomes_esc_sequence() {
        let mut buffer = StdinBuffer::new();
        // Byte 129 = 0x81 → 129 - 128 = 1 = Ctrl+A → \x1b\x01
        let events = buffer.process_bytes(&[0x81]);
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Key(ke) => {
                assert_eq!(ke.key, Key::Char("a".to_string()));
                assert!(ke.modifiers.contains(KeyModifiers::CTRL));
            }
            _ => panic!("expected key event"),
        }
    }

    #[test]
    fn normal_bytes_pass_through_without_conversion() {
        let mut buffer = StdinBuffer::new();
        let events = buffer.process_bytes(&[0x68, 0x69]);
        assert_eq!(events.len(), 2);
        match &events[0] {
            InputEvent::Key(ke) => {
                assert_eq!(ke.key, Key::Char("h".to_string()));
                assert!(ke.modifiers.is_empty());
            }
            _ => panic!("expected key event for 'h'"),
        }
    }
}
