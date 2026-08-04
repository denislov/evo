use std::time::Duration;

use unicode_segmentation::UnicodeSegmentation;

use crate::render::visible_width;
use crate::terminal::{Terminal, TerminalSize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOp {
    Start,
    Stop,
    DrainInput { max_ms: u64, idle_ms: u64 },
    SetTitle(String),
    SetProgress(bool),
    SetKittyProtocolActive(bool),
    Write(String),
    MoveBy(i16),
    MoveToColumn(usize),
    HideCursor,
    ShowCursor,
    ClearLine,
    ClearFromCursor,
    ClearScreen,
    Flush,
}

pub struct VirtualTerminal {
    size: TerminalSize,
    ops: Vec<TerminalOp>,
    writes: Vec<String>,
    kitty_protocol_active: bool,
    cursor_row: usize,
    cursor_col: usize,
    cursor_visible: bool,
    clear_screen_count: usize,
}

impl VirtualTerminal {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            size: TerminalSize { columns, rows },
            ops: Vec::new(),
            writes: Vec::new(),
            kitty_protocol_active: false,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            clear_screen_count: 0,
        }
    }

    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.size = TerminalSize { columns, rows };
    }

    pub fn ops(&self) -> &[TerminalOp] {
        &self.ops
    }

    pub fn clear_ops(&mut self) {
        self.ops.clear();
        self.writes.clear();
    }

    pub fn written_output(&self) -> String {
        self.ops
            .iter()
            .filter_map(|op| match op {
                TerminalOp::Write(data) => Some(data.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn writes(&self) -> &[String] {
        &self.writes
    }

    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn clear_screen_count(&self) -> usize {
        self.clear_screen_count
    }

    pub fn set_kitty_protocol_active(&mut self, active: bool) {
        self.kitty_protocol_active = active;
        self.ops.push(TerminalOp::SetKittyProtocolActive(active));
    }

    fn apply_write_state(&mut self, data: &str) {
        let mut graphemes = data.grapheme_indices(true).peekable();
        while let Some((_, grapheme)) = graphemes.next() {
            if grapheme == "\x1b" {
                skip_escape_sequence(&mut graphemes);
                continue;
            }
            match grapheme {
                "\r" => self.cursor_col = 0,
                "\n" => {
                    self.cursor_row = self.cursor_row.saturating_add(1);
                    self.cursor_col = 0;
                }
                _ if grapheme.chars().all(char::is_control) => {}
                _ => self.cursor_col = self.cursor_col.saturating_add(visible_width(grapheme)),
            }
        }
    }
}

fn skip_escape_sequence<'a, I>(graphemes: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let Some((_, introducer)) = graphemes.next() else {
        return;
    };
    match introducer {
        "[" => {
            for (_, grapheme) in graphemes.by_ref() {
                if grapheme
                    .as_bytes()
                    .last()
                    .is_some_and(|byte| (0x40..=0x7e).contains(byte))
                {
                    break;
                }
            }
        }
        "]" => {
            let mut previous_escape = false;
            for (_, grapheme) in graphemes.by_ref() {
                if grapheme == "\x07" || (previous_escape && grapheme == "\\") {
                    break;
                }
                previous_escape = grapheme == "\x1b";
            }
        }
        _ => {}
    }
}

impl Terminal for VirtualTerminal {
    fn size(&self) -> TerminalSize {
        self.size
    }

    fn write(&mut self, data: &str) -> std::io::Result<()> {
        self.ops.push(TerminalOp::Write(data.to_string()));
        self.writes.push(data.to_string());
        self.apply_write_state(data);
        Ok(())
    }

    fn move_by(&mut self, rows: i16) -> std::io::Result<()> {
        self.ops.push(TerminalOp::MoveBy(rows));
        if rows < 0 {
            self.cursor_row = self.cursor_row.saturating_sub((-rows) as usize);
        } else {
            self.cursor_row = self.cursor_row.saturating_add(rows as usize);
        }
        Ok(())
    }

    fn move_to_column(&mut self, column: usize) -> std::io::Result<()> {
        self.ops.push(TerminalOp::MoveToColumn(column));
        self.cursor_col = column;
        Ok(())
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::HideCursor);
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::ShowCursor);
        self.cursor_visible = true;
        Ok(())
    }

    fn clear_line(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::ClearLine);
        Ok(())
    }

    fn clear_from_cursor(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::ClearFromCursor);
        Ok(())
    }

    fn clear_screen(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::ClearScreen);
        self.clear_screen_count = self.clear_screen_count.saturating_add(1);
        self.cursor_row = 0;
        self.cursor_col = 0;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::Flush);
        Ok(())
    }

    fn start(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::Start);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.ops.push(TerminalOp::Stop);
        Ok(())
    }

    fn drain_input(&mut self, max: Duration, idle: Duration) -> std::io::Result<()> {
        self.ops.push(TerminalOp::DrainInput {
            max_ms: max.as_millis() as u64,
            idle_ms: idle.as_millis() as u64,
        });
        Ok(())
    }

    fn set_title(&mut self, title: &str) -> std::io::Result<()> {
        self.ops.push(TerminalOp::SetTitle(title.to_string()));
        Ok(())
    }

    fn set_progress(&mut self, active: bool) -> std::io::Result<()> {
        self.ops.push(TerminalOp::SetProgress(active));
        Ok(())
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }
}
