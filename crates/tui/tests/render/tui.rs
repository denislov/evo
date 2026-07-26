//! TUI rendering strategies and terminal operation behavior.

use std::sync::{Arc, Mutex};

use tui::api::component::{CURSOR_MARKER, Component, Image, OverlayOptions, Text};
use tui::api::render::{RenderStrategy, Tui, TuiError};
use tui::api::terminal::{
    ImageDimensions, ImageProtocol, Terminal, TerminalCapabilities, TerminalMode, TerminalSize,
};
use tui::api::testing::{TerminalOp, VirtualTerminal};

struct RawComponent {
    lines: Vec<String>,
}

impl RawComponent {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|line| line.to_string()).collect(),
        }
    }
}

impl Component for RawComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
}

struct LifecycleTerminal {
    events: Arc<Mutex<Vec<&'static str>>>,
    fail_start: bool,
}

#[derive(Default)]
struct FaultState {
    operation: usize,
    fail_at: Option<usize>,
    panic_at: Option<usize>,
    size: Option<TerminalSize>,
    writes: Vec<String>,
    progress: Vec<bool>,
    stop_attempts: usize,
    stopped: bool,
}

struct FaultTerminal {
    state: Arc<Mutex<FaultState>>,
}

impl FaultTerminal {
    fn operation(&mut self, write: Option<&str>) -> std::io::Result<()> {
        let (operation, should_panic, should_fail) = {
            let mut state = self.state.lock().unwrap();
            state.operation += 1;
            (
                state.operation,
                state.panic_at == Some(state.operation),
                state.fail_at == Some(state.operation),
            )
        };
        assert!(
            !should_panic,
            "injected terminal panic at operation {operation}"
        );
        if should_fail {
            return Err(std::io::Error::other(format!(
                "injected terminal failure at operation {operation}"
            )));
        }
        if let Some(write) = write {
            self.state.lock().unwrap().writes.push(write.to_string());
        }
        Ok(())
    }
}

impl Terminal for FaultTerminal {
    fn size(&self) -> TerminalSize {
        self.state.lock().unwrap().size.unwrap_or(TerminalSize {
            columns: 20,
            rows: 5,
        })
    }

    fn write(&mut self, data: &str) -> std::io::Result<()> {
        self.operation(Some(data))
    }

    fn move_by(&mut self, _rows: i16) -> std::io::Result<()> {
        self.operation(None)
    }

    fn move_to_column(&mut self, _column: usize) -> std::io::Result<()> {
        self.operation(None)
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn clear_line(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn clear_from_cursor(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn clear_screen(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.operation(None)
    }

    fn start_mode(&mut self, _mode: TerminalMode) -> std::io::Result<()> {
        self.operation(None)
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.state.lock().unwrap().stop_attempts += 1;
        self.operation(None)?;
        self.state.lock().unwrap().stopped = true;
        Ok(())
    }

    fn set_progress(&mut self, active: bool) -> std::io::Result<()> {
        self.operation(None)?;
        self.state.lock().unwrap().progress.push(active);
        Ok(())
    }
}

impl Terminal for LifecycleTerminal {
    fn size(&self) -> TerminalSize {
        TerminalSize {
            columns: 20,
            rows: 5,
        }
    }

    fn write(&mut self, _data: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn move_by(&mut self, _rows: i16) -> std::io::Result<()> {
        Ok(())
    }

    fn move_to_column(&mut self, _column: usize) -> std::io::Result<()> {
        Ok(())
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clear_line(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clear_from_cursor(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clear_screen(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn start_mode(&mut self, mode: TerminalMode) -> std::io::Result<()> {
        assert_eq!(mode, TerminalMode::Fullscreen);
        self.events.lock().unwrap().push("start");
        if self.fail_start {
            return Err(std::io::Error::other("injected start failure"));
        }
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.events.lock().unwrap().push("stop");
        Ok(())
    }
}

fn assert_no_global_clear(terminal: &VirtualTerminal) {
    assert!(!terminal.ops().contains(&TerminalOp::ClearScreen));
    assert!(!terminal.ops().contains(&TerminalOp::ClearFromCursor));
    let written = terminal.written_output();
    assert!(!written.contains("\x1b[2J"));
    assert!(!written.contains("\x1b[3J"));
    assert!(!written.contains("\x1b[H"));
}

#[test]
fn first_render_appends_inline_without_clearing_or_homing() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(Text::new("hello")));

    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::FullRedraw);
    assert_eq!(outcome.line_count, 1);
    assert_eq!(tui.full_redraws(), 1);
    assert!(tui.terminal().ops().contains(&TerminalOp::HideCursor));
    assert_no_global_clear(tui.terminal());
    assert!(tui.terminal().written_output().contains("\x1b[?2026h"));
    assert!(
        tui.terminal()
            .written_output()
            .contains("hello\x1b[0m\x1b]8;;\x07")
    );
    assert!(tui.terminal().written_output().contains("\x1b[?2026l"));
}

#[test]
fn fullscreen_render_owns_exact_frame_and_bottom_aligns_existing_layout() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.add_child(Box::new(RawComponent::new(&["one", "two"])));

    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::FullRedraw);
    assert_eq!(outcome.line_count, 5);
    assert_eq!(tui.rendered_lines(), &["", "", "", "one", "two"]);
    assert_eq!(tui.terminal_mode(), TerminalMode::Fullscreen);
    assert!(
        tui.terminal()
            .ops()
            .contains(&TerminalOp::Start(TerminalMode::Fullscreen))
    );
    assert!(tui.terminal().ops().contains(&TerminalOp::ClearScreen));

    tui.stop().unwrap();
    assert!(tui.terminal().ops().contains(&TerminalOp::Stop));
}

#[test]
fn fullscreen_resize_rebuilds_bounded_frame_without_scrolling() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.add_child(Box::new(RawComponent::new(&[
        "one", "two", "three", "four", "five", "six",
    ])));
    tui.render_once().unwrap();
    assert_eq!(
        tui.rendered_lines(),
        &["two", "three", "four", "five", "six"]
    );
    tui.terminal_mut().clear_ops();
    tui.terminal_mut().resize(20, 3);

    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::FullRedraw);
    assert_eq!(outcome.line_count, 3);
    assert_eq!(tui.rendered_lines(), &["four", "five", "six"]);
    assert!(tui.terminal().ops().contains(&TerminalOp::ClearScreen));
    assert_eq!(tui.terminal().written_output().matches("\r\n").count(), 2);
}

#[test]
fn tui_start_failure_stops_partially_started_terminal() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let terminal = LifecycleTerminal {
        events: Arc::clone(&events),
        fail_start: true,
    };

    let result = Tui::start(terminal, TerminalMode::Fullscreen);

    assert!(matches!(result, Err(TuiError::Io(_))));
    assert_eq!(&*events.lock().unwrap(), &["start", "stop"]);
}

#[test]
fn tui_drop_restores_terminal_during_panic_unwind() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let terminal = LifecycleTerminal {
        events: Arc::clone(&events),
        fail_start: false,
    };

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
        panic!("injected panic");
    }));

    assert!(unwind.is_err());
    assert_eq!(&*events.lock().unwrap(), &["start", "stop"]);
}

#[test]
fn tui_drop_restores_terminal_after_runtime_render_error() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let terminal = LifecycleTerminal {
        events: Arc::clone(&events),
        fail_start: false,
    };
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.add_child(Box::new(RawComponent::new(&[
        "this line exceeds the terminal width",
    ])));

    assert!(matches!(
        tui.render_once(),
        Err(TuiError::LineTooWide { .. })
    ));
    drop(tui);

    assert_eq!(&*events.lock().unwrap(), &["start", "stop"]);
}

#[test]
fn synchronized_full_redraw_ends_after_each_injected_io_failure() {
    const SYNC_START: &str = "\x1b[?2026h";
    const SYNC_END: &str = "\x1b[?2026l";

    for fail_at in 3..=12 {
        let state = Arc::new(Mutex::new(FaultState {
            fail_at: Some(fail_at),
            ..Default::default()
        }));
        let terminal = FaultTerminal {
            state: Arc::clone(&state),
        };
        let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
        tui.add_child(Box::new(RawComponent::new(&["one", "two"])));

        let _ = tui.render_once();
        let _ = tui.stop();
        drop(tui);

        let state = state.lock().unwrap();
        if state.writes.iter().any(|write| write == SYNC_START) {
            assert!(
                state.writes.iter().any(|write| write == SYNC_END),
                "failure at operation {fail_at} left synchronized output active: {:?}",
                state.writes
            );
        }
        assert!(state.stopped, "failure at operation {fail_at} skipped stop");
    }
}

#[test]
fn synchronized_full_redraw_ends_before_resuming_terminal_panic() {
    const SYNC_END: &str = "\x1b[?2026l";
    let state = Arc::new(Mutex::new(FaultState {
        panic_at: Some(3),
        ..Default::default()
    }));
    let terminal = FaultTerminal {
        state: Arc::clone(&state),
    };
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.add_child(Box::new(RawComponent::new(&["panic frame"])));

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tui.render_once()));

    assert!(unwind.is_err());
    assert!(
        state
            .lock()
            .unwrap()
            .writes
            .iter()
            .any(|write| write == SYNC_END)
    );
}

#[test]
fn synchronized_differential_redraw_balances_each_injected_io_failure() {
    const SYNC_START: &str = "\x1b[?2026h";
    const SYNC_END: &str = "\x1b[?2026l";

    for fail_offset in 1..=12 {
        let state = Arc::new(Mutex::new(FaultState::default()));
        let terminal = FaultTerminal {
            state: Arc::clone(&state),
        };
        let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
        tui.add_child(Box::new(RawComponent::new(&["header", "before"])));
        tui.render_once().unwrap();
        tui.clear_children();
        tui.add_child(Box::new(RawComponent::new(&["header", "after"])));
        {
            let mut state = state.lock().unwrap();
            state.fail_at = Some(state.operation + fail_offset);
        }

        let _ = tui.render_once();
        let _ = tui.stop();
        drop(tui);

        let state = state.lock().unwrap();
        let starts = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_START)
            .count();
        let ends = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_END)
            .count();
        assert_eq!(starts, ends, "unbalanced sync at offset {fail_offset}");
        assert!(
            state.stopped,
            "failure at offset {fail_offset} skipped stop"
        );
    }
}

#[test]
fn overlay_redraw_balances_sync_after_each_injected_io_failure() {
    const SYNC_START: &str = "\x1b[?2026h";
    const SYNC_END: &str = "\x1b[?2026l";

    for fail_offset in 1..=12 {
        let state = Arc::new(Mutex::new(FaultState::default()));
        let terminal = FaultTerminal {
            state: Arc::clone(&state),
        };
        let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
        tui.add_child(Box::new(RawComponent::new(&["base"])));
        tui.render_once().unwrap();
        tui.show_overlay(
            Box::new(RawComponent::new(&["overlay"])),
            OverlayOptions::default(),
        );
        {
            let mut state = state.lock().unwrap();
            state.fail_at = Some(state.operation + fail_offset);
        }

        let _ = tui.render_once();
        let _ = tui.stop();
        drop(tui);

        let state = state.lock().unwrap();
        let starts = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_START)
            .count();
        let ends = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_END)
            .count();
        assert_eq!(
            starts, ends,
            "unbalanced overlay sync at offset {fail_offset}"
        );
        assert!(
            state.stopped,
            "overlay failure at offset {fail_offset} skipped stop"
        );
    }
}

#[test]
fn resize_redraw_balances_sync_after_each_injected_io_failure() {
    const SYNC_START: &str = "\x1b[?2026h";
    const SYNC_END: &str = "\x1b[?2026l";

    for fail_offset in 1..=12 {
        let state = Arc::new(Mutex::new(FaultState::default()));
        let terminal = FaultTerminal {
            state: Arc::clone(&state),
        };
        let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
        tui.add_child(Box::new(RawComponent::new(&["one", "two", "three"])));
        tui.render_once().unwrap();
        {
            let mut state = state.lock().unwrap();
            state.size = Some(TerminalSize {
                columns: 16,
                rows: 3,
            });
            state.fail_at = Some(state.operation + fail_offset);
        }

        let _ = tui.render_once();
        let _ = tui.stop();
        drop(tui);

        let state = state.lock().unwrap();
        let starts = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_START)
            .count();
        let ends = state
            .writes
            .iter()
            .filter(|write| write.as_str() == SYNC_END)
            .count();
        assert_eq!(
            starts, ends,
            "unbalanced resize sync at offset {fail_offset}"
        );
        assert!(
            state.stopped,
            "resize failure at offset {fail_offset} skipped stop"
        );
    }
}

#[test]
fn failed_stop_is_retried_by_drop_after_progress_was_active() {
    let state = Arc::new(Mutex::new(FaultState::default()));
    let terminal = FaultTerminal {
        state: Arc::clone(&state),
    };
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.terminal_mut().set_progress(true).unwrap();
    {
        let mut state = state.lock().unwrap();
        state.fail_at = Some(state.operation + 1);
    }

    assert!(tui.stop().is_err());
    drop(tui);

    let state = state.lock().unwrap();
    assert_eq!(state.progress, &[true]);
    assert_eq!(state.stop_attempts, 2);
    assert!(state.stopped);
}

#[test]
fn full_render_writes_lines_with_carriage_return_newline() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["one", "two"])));

    tui.render_once().unwrap();

    assert!(
        tui.terminal()
            .written_output()
            .contains("one\x1b[0m\x1b]8;;\x07\r\ntwo")
    );
}

#[test]
fn line_too_wide_errors_before_writing() {
    let terminal = VirtualTerminal::new(4, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["too wide"])));

    let err = tui.render_once().unwrap_err();

    match err {
        TuiError::LineTooWide {
            line_index,
            width,
            max_width,
            ..
        } => {
            assert_eq!(line_index, 0);
            assert_eq!(width, 8);
            assert_eq!(max_width, 4);
        }
        other => panic!("expected LineTooWide, got {other:?}"),
    }
    assert!(tui.terminal().ops().is_empty());
}

#[test]
fn zero_sized_terminal_suspends_render_until_a_nonzero_resize() {
    let terminal = VirtualTerminal::new(0, 0);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["content"])));

    let suspended = tui.render_once().unwrap();

    assert_eq!(suspended.strategy, RenderStrategy::NoChange);
    assert_eq!(suspended.line_count, 0);
    assert!(tui.terminal().ops().is_empty());

    tui.terminal_mut().resize(20, 5);
    let resumed = tui.render_once().unwrap();
    assert_eq!(resumed.strategy, RenderStrategy::FullRedraw);
    assert!(tui.terminal().written_output().contains("content"));
}

#[test]
fn differential_render_returns_to_column_zero_before_clearing() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["header", "working"])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["header", "done"])));
    tui.render_once().unwrap();

    let ops = tui.terminal().ops();
    let move_to_column = ops
        .iter()
        .position(|op| *op == TerminalOp::MoveToColumn(0))
        .expect("expected differential render to return to column zero");
    let clear_line = ops
        .iter()
        .position(|op| *op == TerminalOp::ClearLine)
        .expect("expected differential render to clear the changed owned line");
    assert!(move_to_column < clear_line);
    assert_no_global_clear(tui.terminal());
}

#[test]
fn differential_render_moves_from_actual_hardware_cursor_row() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent {
        lines: vec![format!("a{CURSOR_MARKER}bc"), "old".to_string()],
    }));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent {
        lines: vec![format!("a{CURSOR_MARKER}bc"), "new".to_string()],
    }));
    tui.render_once().unwrap();

    assert!(tui.terminal().ops().contains(&TerminalOp::MoveBy(1)));
}

#[test]
fn unchanged_render_still_repositions_hardware_cursor() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent {
        lines: vec![format!("a{CURSOR_MARKER}bc")],
    }));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent {
        lines: vec![format!("ab{CURSOR_MARKER}c")],
    }));
    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::NoChange);
    let ops = tui.terminal().ops();
    let move_to_column = ops
        .iter()
        .position(|op| *op == TerminalOp::MoveToColumn(2))
        .expect("expected cursor movement to move to marker column");
    ops[move_to_column + 1..]
        .iter()
        .position(|op| *op == TerminalOp::Flush)
        .expect("expected marker-only cursor movement to flush");
}

#[test]
fn second_render_updates_from_first_changed_line_without_full_clear() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&[
        "header", "working", "footer",
    ])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["header", "done", "footer"])));
    let outcome = tui.render_once().unwrap();

    assert_eq!(
        outcome.strategy,
        RenderStrategy::Differential {
            first_changed_line: 1,
            last_changed_line: 1
        }
    );
    assert_no_global_clear(tui.terminal());
    assert!(tui.terminal().ops().contains(&TerminalOp::MoveBy(-1)));
    assert!(tui.terminal().ops().contains(&TerminalOp::ClearLine));
    assert!(tui.terminal().written_output().contains("done"));
}

#[test]
fn differential_render_writes_growth_beyond_terminal_height() {
    let terminal = VirtualTerminal::new(20, 4);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["welcome", "> ", "footer"])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&[
        "welcome", "one", "two", "three", "four", "five", "six", "> ", "footer",
    ])));
    let outcome = tui.render_once().unwrap();

    assert_eq!(
        outcome.strategy,
        RenderStrategy::Differential {
            first_changed_line: 1,
            last_changed_line: 8
        }
    );
    assert_no_global_clear(tui.terminal());
    let written = tui.terminal().written_output();
    assert!(written.contains("one"), "{written:?}");
    assert!(written.contains("six"), "{written:?}");
    assert!(written.contains("footer"), "{written:?}");
}

#[test]
fn differential_render_only_rewrites_changed_range() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&[
        "header", "working", "footer",
    ])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["header", "done", "footer"])));
    let outcome = tui.render_once().unwrap();

    assert_eq!(
        outcome.strategy,
        RenderStrategy::Differential {
            first_changed_line: 1,
            last_changed_line: 1
        }
    );
    let written = tui.terminal().written_output();
    assert!(written.contains("done"), "{written:?}");
    assert!(!written.contains("footer"), "{written:?}");
}

#[test]
fn kitty_image_cleanup_tracks_latest_rendered_frame() {
    let terminal = VirtualTerminal::new(80, 10);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(RawComponent::new(&["\x1b_Gi=42;payload\x1b\\"])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["plain"])));
    tui.render_once().unwrap();

    let written = tui.terminal().written_output();
    assert!(
        written.contains("\x1b_Ga=d,d=I,i=42,q=2\x1b\\"),
        "{written:?}"
    );

    tui.terminal_mut().clear_ops();
    tui.render_once().unwrap();
    let written = tui.terminal().written_output();
    assert!(
        !written.contains("\x1b_Ga=d,d=I,i=42,q=2\x1b\\"),
        "{written:?}"
    );
}

#[test]
fn failed_kitty_image_deletion_is_retried_on_the_next_frame() {
    const DELETE_IMAGE_42: &str = "\x1b_Ga=d,d=I,i=42,q=2\x1b\\";
    let state = Arc::new(Mutex::new(FaultState::default()));
    let terminal = FaultTerminal {
        state: Arc::clone(&state),
    };
    let mut tui = Tui::start(terminal, TerminalMode::Fullscreen).unwrap();
    tui.add_child(Box::new(RawComponent::new(&["\x1b_Gi=42;payload\x1b\\"])));
    tui.render_once().unwrap();
    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["plain"])));
    {
        let mut state = state.lock().unwrap();
        state.fail_at = Some(state.operation + 2);
    }

    assert!(tui.render_once().is_err());
    tui.render_once().unwrap();

    assert!(
        state
            .lock()
            .unwrap()
            .writes
            .iter()
            .any(|write| write == DELETE_IMAGE_42)
    );
}

#[test]
fn image_component_reserves_its_protocol_reported_rows_before_following_content() {
    let terminal = VirtualTerminal::new(80, 10);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(
        Image::new("payload", "image/png")
            .dimensions(ImageDimensions {
                width_px: 18,
                height_px: 18,
            })
            .max_width_cells(10)
            .image_id(7)
            .capabilities(TerminalCapabilities {
                images: Some(ImageProtocol::Kitty),
                true_color: true,
                hyperlinks: true,
            }),
    ));
    tui.add_child(Box::new(Text::new("after image")));

    tui.render_once().unwrap();

    assert_eq!(tui.rendered_lines().len(), 6);
    assert!(tui.rendered_lines()[0].contains("c=10,r=5,i=7"));
    assert!(tui.rendered_lines()[1..5].iter().all(String::is_empty));
    assert!(tui.rendered_lines()[5].contains("after image"));
}

#[test]
fn width_change_triggers_scoped_redraw_without_global_clear() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(Text::new("hello")));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();
    tui.terminal_mut().resize(30, 5);

    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::FullRedraw);
    assert_no_global_clear(tui.terminal());
    assert!(tui.terminal().ops().contains(&TerminalOp::ClearLine));
    assert!(tui.terminal().written_output().contains("hello"));
}

#[test]
fn shrink_with_clear_on_shrink_clears_only_owned_rows() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.set_clear_on_shrink(true);
    tui.add_child(Box::new(RawComponent::new(&["one", "two", "three"])));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    tui.clear_children();
    tui.add_child(Box::new(RawComponent::new(&["one"])));
    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::FullRedraw);
    assert_no_global_clear(tui.terminal());
    let cleared_rows = tui
        .terminal()
        .ops()
        .iter()
        .filter(|op| **op == TerminalOp::ClearLine)
        .count();
    assert_eq!(cleared_rows, 3);
    assert!(tui.terminal().written_output().contains("one"));
    assert!(!tui.terminal().written_output().contains("two"));
}

#[test]
fn unchanged_render_reports_no_change() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(Text::new("hello")));
    tui.render_once().unwrap();
    tui.terminal_mut().clear_ops();

    let outcome = tui.render_once().unwrap();

    assert_eq!(outcome.strategy, RenderStrategy::NoChange);
    assert!(tui.terminal().ops().is_empty());
}
