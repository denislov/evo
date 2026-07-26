use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::ansi::ansi_sequence_len;

pub fn visible_width(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut clean = String::new();
    let mut pos = 0;

    while pos < text.len() {
        if let Some(len) = ansi_sequence_len(text, pos) {
            pos += len;
            continue;
        }

        let ch = text[pos..]
            .chars()
            .next()
            .expect("pos is on a char boundary");
        if ch == '\t' {
            clean.push_str("   ");
        } else {
            clean.push(ch);
        }
        pos += ch.len_utf8();
    }

    clean.graphemes(true).map(UnicodeWidthStr::width).sum()
}

pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 || text.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    let mut width = 0;
    let mut pos = 0;

    while pos < text.len() {
        if let Some(len) = ansi_sequence_len(text, pos) {
            output.push_str(&text[pos..pos + len]);
            pos += len;
            continue;
        }

        let ch = text[pos..]
            .chars()
            .next()
            .expect("pos is on a char boundary");
        if ch == '\t' {
            if width + 3 > max_width {
                break;
            }
            output.push(ch);
            width += 3;
            pos += ch.len_utf8();
            continue;
        }

        let mut graphemes = text[pos..].graphemes(true);
        let grapheme = graphemes.next().expect("grapheme exists");
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width {
            break;
        }

        output.push_str(grapheme);
        width += grapheme_width;
        pos += grapheme.len();
    }

    output
}

pub(crate) fn drop_columns(text: &str, columns: usize) -> String {
    if columns == 0 || text.is_empty() {
        return text.to_string();
    }

    let mut output = String::new();
    let mut skipped = 0usize;
    let mut pos = 0usize;
    while pos < text.len() {
        if let Some(len) = ansi_sequence_len(text, pos) {
            output.push_str(&text[pos..pos + len]);
            pos += len;
            continue;
        }

        let ch = text[pos..]
            .chars()
            .next()
            .expect("pos is on a char boundary");
        if ch == '\t' {
            if skipped.saturating_add(3) > columns {
                output.push(ch);
            }
            skipped = skipped.saturating_add(3);
            pos += ch.len_utf8();
            continue;
        }

        let grapheme = text[pos..].graphemes(true).next().expect("grapheme exists");
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if skipped.saturating_add(grapheme_width) > columns {
            output.push_str(grapheme);
        }
        skipped = skipped.saturating_add(grapheme_width);
        pos += grapheme.len();
    }
    output
}

pub fn truncate_to_width_with_ellipsis(text: &str, max_width: usize) -> String {
    if visible_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let prefix = truncate_to_width(text, max_width - 3);
    let mut output = prefix.clone();
    output.push_str("...");
    if active_sgr_codes(&prefix).is_some() && !prefix.ends_with("\x1b[0m") {
        output.push_str("\x1b[0m");
    }
    output
}

pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return text.split('\n').map(|_| String::new()).collect();
    }

    let mut lines = Vec::new();
    let mut active = AnsiState::default();
    for (index, input_line) in text.split('\n').enumerate() {
        let line = if index == 0 {
            input_line.to_string()
        } else {
            format!("{}{}", active.active_codes(), input_line)
        };
        lines.extend(wrap_single_ansi_line(&line, width));
        active.update_from_text(input_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn wrap_single_ansi_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let tokens = tokenize_ansi_words(line);
    let mut active = AnsiState::default();
    let mut output = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for token in tokens {
        if token.is_ansi {
            active.update_from_text(&token.text);
            current.push_str(&token.text);
            continue;
        }

        let token_width = visible_width(&token.text);
        let whitespace = token.text.trim().is_empty();
        if token_width > width && !whitespace {
            if current_width > 0 {
                output.push(trim_trailing_visible_space(&current));
                current = active.active_codes();
                current_width = 0;
            }
            let broken = break_long_visible_token(&token.text, width, &mut active);
            let broken_len = broken.len();
            for (index, line) in broken.into_iter().enumerate() {
                if index + 1 == broken_len && visible_width(&line) < width {
                    current = line;
                    current_width = visible_width(&current);
                } else {
                    output.push(line);
                }
            }
            continue;
        }

        if current_width + token_width > width && current_width > 0 {
            output.push(trim_trailing_visible_space(&current));
            current = active.active_codes();
            current_width = 0;
            if whitespace {
                continue;
            }
        }

        if !(current_width == 0 && whitespace) {
            current.push_str(&token.text);
            current_width += token_width;
        }
    }

    if !current.is_empty() || output.is_empty() {
        output.push(trim_trailing_visible_space(&current));
    }
    output
}

fn break_long_visible_token(token: &str, width: usize, active: &mut AnsiState) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = active.active_codes();
    let mut current_width = 0usize;
    let mut pos = 0usize;

    while pos < token.len() {
        if let Some(len) = ansi_sequence_len(token, pos) {
            let seq = &token[pos..pos + len];
            active.update_from_text(seq);
            current.push_str(seq);
            pos += len;
            continue;
        }

        let grapheme = token[pos..].graphemes(true).next().unwrap();
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width + grapheme_width > width && current_width > 0 {
            output.push(current);
            current = active.active_codes();
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
        pos += grapheme.len();
    }

    if !current.is_empty() {
        output.push(current);
    }
    output
}

#[derive(Debug)]
struct AnsiToken {
    text: String,
    is_ansi: bool,
}

fn tokenize_ansi_words(text: &str) -> Vec<AnsiToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_is_space: Option<bool> = None;
    let mut pos = 0usize;

    while pos < text.len() {
        if let Some(len) = ansi_sequence_len(text, pos) {
            if !current.is_empty() {
                tokens.push(AnsiToken {
                    text: std::mem::take(&mut current),
                    is_ansi: false,
                });
                current_is_space = None;
            }
            tokens.push(AnsiToken {
                text: text[pos..pos + len].to_string(),
                is_ansi: true,
            });
            pos += len;
            continue;
        }

        let ch = text[pos..].chars().next().unwrap();
        let is_space = ch.is_whitespace();
        if let Some(existing) = current_is_space
            && existing != is_space
            && !current.is_empty()
        {
            tokens.push(AnsiToken {
                text: std::mem::take(&mut current),
                is_ansi: false,
            });
        }
        current_is_space = Some(is_space);
        current.push(ch);
        pos += ch.len_utf8();
    }

    if !current.is_empty() {
        tokens.push(AnsiToken {
            text: current,
            is_ansi: false,
        });
    }
    tokens
}

fn trim_trailing_visible_space(text: &str) -> String {
    let mut cut = text.len();
    while cut > 0 {
        let Some(ch) = text[..cut].chars().next_back() else {
            break;
        };
        if ch == ' ' || ch == '\t' {
            cut -= ch.len_utf8();
        } else {
            break;
        }
    }
    text[..cut].to_string()
}

#[derive(Default)]
struct AnsiState {
    active: Vec<String>,
}

impl AnsiState {
    fn active_codes(&self) -> String {
        self.active.join("")
    }

    fn update_from_text(&mut self, text: &str) {
        let mut pos = 0usize;
        while pos < text.len() {
            if let Some(len) = ansi_sequence_len(text, pos) {
                let seq = &text[pos..pos + len];
                self.update_from_sequence(seq);
                pos += len;
            } else {
                let ch = text[pos..].chars().next().unwrap();
                pos += ch.len_utf8();
            }
        }
    }

    fn update_from_sequence(&mut self, seq: &str) {
        if !seq.starts_with("\x1b[") || !seq.ends_with('m') {
            return;
        }
        let body = &seq[2..seq.len() - 1];
        if body.is_empty() || body.split(';').any(|part| part == "0") {
            self.active.clear();
        } else {
            self.active.push(seq.to_string());
        }
    }
}

fn active_sgr_codes(text: &str) -> Option<String> {
    let mut state = AnsiState::default();
    state.update_from_text(text);
    let active = state.active_codes();
    (!active.is_empty()).then_some(active)
}
