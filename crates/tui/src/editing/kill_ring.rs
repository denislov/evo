#[derive(Debug, Clone, Default)]
pub struct KillRing {
    entries: Vec<String>,
    cursor: usize,
}

impl KillRing {
    pub fn push(&mut self, text: impl Into<String>, prepend: bool, accumulate: bool) {
        let text = text.into();
        if text.is_empty() {
            return;
        }

        if accumulate && !self.entries.is_empty() {
            if prepend {
                self.entries[0] = format!("{text}{}", self.entries[0]);
            } else {
                self.entries[0].push_str(&text);
            }
        } else {
            self.entries.insert(0, text);
        }
        self.cursor = 0;
    }

    pub fn yank(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(String::as_str)
    }

    pub fn yank_pop(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        self.cursor = (self.cursor + 1) % self.entries.len();
        self.yank()
    }
}

#[cfg(test)]
mod tests {
    use super::KillRing;

    #[test]
    fn accumulates_and_rotates_entries() {
        let mut ring = KillRing::default();
        ring.push("one", false, false);
        ring.push("two", false, false);
        assert_eq!(ring.yank(), Some("two"));
        assert_eq!(ring.yank_pop(), Some("one"));
    }
}
