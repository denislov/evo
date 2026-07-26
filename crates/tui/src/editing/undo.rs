#[derive(Debug, Clone)]
pub struct UndoStack<T> {
    entries: Vec<T>,
    max_entries: usize,
}

impl<T: Clone> UndoStack<T> {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
        }
    }

    pub fn push(&mut self, state: T) {
        self.entries.push(state);
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        self.entries.pop()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn undo(&mut self, current: T) -> T {
        self.entries.pop().unwrap_or(current)
    }
}

impl<T: Clone> Default for UndoStack<T> {
    fn default() -> Self {
        Self::new(100)
    }
}

#[cfg(test)]
mod tests {
    use super::UndoStack;

    #[test]
    fn returns_last_state_or_current_when_empty() {
        let mut stack = UndoStack::new(2);
        assert_eq!(stack.undo("now"), "now");
        stack.push("before");
        assert_eq!(stack.undo("now"), "before");
    }
}
