use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::interactive) struct TranscriptBlockId {
    pub(super) transcript_id: u64,
    pub(super) item_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::interactive) enum TranscriptDisplayState {
    Collapsed,
    Preview,
    Expanded,
}

impl TranscriptDisplayState {
    fn next(self) -> Self {
        match self {
            Self::Collapsed => Self::Preview,
            Self::Preview => Self::Expanded,
            Self::Expanded => Self::Collapsed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::interactive) struct TranscriptViewSnapshot {
    revision: u64,
    selected: Option<TranscriptBlockId>,
    display_states: HashMap<TranscriptBlockId, TranscriptDisplayState>,
    tool_argument_states: HashMap<TranscriptBlockId, TranscriptDisplayState>,
}

impl TranscriptViewSnapshot {
    pub(in crate::interactive) fn revision(&self) -> u64 {
        self.revision
    }

    pub(in crate::interactive) fn display_state(
        &self,
        block_id: TranscriptBlockId,
        item: &TranscriptItem,
    ) -> TranscriptDisplayState {
        self.display_states
            .get(&block_id)
            .copied()
            .unwrap_or_else(|| default_display_state(item))
    }

    pub(in crate::interactive) fn tool_argument_state(
        &self,
        block_id: TranscriptBlockId,
        item: &TranscriptItem,
    ) -> TranscriptDisplayState {
        self.tool_argument_states
            .get(&block_id)
            .copied()
            .unwrap_or_else(|| default_tool_argument_state(item))
    }
}

#[derive(Debug, Default)]
pub(in crate::interactive) struct TranscriptViewState {
    transcript_id: Option<u64>,
    content_revision: Option<u64>,
    selected: Option<TranscriptBlockId>,
    last_selectable: Option<TranscriptBlockId>,
    display_states: HashMap<TranscriptBlockId, TranscriptDisplayState>,
    tool_argument_states: HashMap<TranscriptBlockId, TranscriptDisplayState>,
    revision: u64,
    cached_snapshot: RefCell<Option<Arc<TranscriptViewSnapshot>>>,
}

impl TranscriptViewState {
    pub(in crate::interactive) fn sync(&mut self, transcript: &Transcript) {
        let transcript_id = transcript.render_cache_id();
        let content_revision = transcript.content_revision();
        if self.transcript_id == Some(transcript_id)
            && self.content_revision == Some(content_revision)
        {
            return;
        }
        let mut changed = false;
        if self.transcript_id != Some(transcript_id) {
            self.transcript_id = Some(transcript_id);
            self.selected = None;
            self.last_selectable = None;
            self.display_states.clear();
            self.tool_argument_states.clear();
            changed = true;
        }

        let entries = transcript
            .view_entries()
            .filter(|(_, item)| item.selectable())
            .collect::<Vec<_>>();
        let visible_ids = entries
            .iter()
            .map(|(block_id, _)| *block_id)
            .collect::<HashSet<_>>();
        let new_last = entries.last().map(|(block_id, _)| *block_id);
        let selected_is_valid = self.selected.is_some_and(|id| visible_ids.contains(&id));
        if (!selected_is_valid || self.selected == self.last_selectable)
            && self.selected != new_last
        {
            self.selected = new_last;
            changed = true;
        }
        self.last_selectable = new_last;

        let before_len = self.display_states.len();
        self.display_states.retain(|id, _| visible_ids.contains(id));
        changed |= self.display_states.len() != before_len;
        let before_len = self.tool_argument_states.len();
        self.tool_argument_states
            .retain(|id, _| visible_ids.contains(id));
        changed |= self.tool_argument_states.len() != before_len;
        if changed {
            self.bump_revision();
        }
        self.content_revision = Some(content_revision);
    }

    pub(in crate::interactive) fn snapshot(&self) -> Arc<TranscriptViewSnapshot> {
        if let Some(snapshot) = self.cached_snapshot.borrow().as_ref() {
            return Arc::clone(snapshot);
        }
        let snapshot = Arc::new(TranscriptViewSnapshot {
            revision: self.revision,
            selected: self.selected,
            display_states: self.display_states.clone(),
            tool_argument_states: self.tool_argument_states.clone(),
        });
        *self.cached_snapshot.borrow_mut() = Some(Arc::clone(&snapshot));
        snapshot
    }

    pub(in crate::interactive) fn selected(&self) -> Option<TranscriptBlockId> {
        self.selected
    }

    pub(in crate::interactive) fn revision(&self) -> u64 {
        self.revision
    }

    pub(in crate::interactive) fn select_previous(&mut self, transcript: &Transcript) -> bool {
        self.move_selection(transcript, -1)
    }

    pub(in crate::interactive) fn select_next(&mut self, transcript: &Transcript) -> bool {
        self.move_selection(transcript, 1)
    }

    pub(in crate::interactive) fn select_first(&mut self, transcript: &Transcript) -> bool {
        self.select_boundary(transcript, false)
    }

    pub(in crate::interactive) fn select_last(&mut self, transcript: &Transcript) -> bool {
        self.select_boundary(transcript, true)
    }

    pub(in crate::interactive) fn select(
        &mut self,
        transcript: &Transcript,
        block_id: TranscriptBlockId,
    ) -> bool {
        let Some(item) = transcript.item_for_block(block_id) else {
            return false;
        };
        if !item.selectable() || self.selected == Some(block_id) {
            return false;
        }
        self.selected = Some(block_id);
        self.bump_revision();
        true
    }

    pub(in crate::interactive) fn toggle_selected(&mut self, transcript: &Transcript) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let Some(item) = transcript.item_for_block(selected) else {
            return false;
        };
        if !item.foldable() {
            return false;
        }
        let current = self
            .display_states
            .get(&selected)
            .copied()
            .unwrap_or_else(|| default_display_state(item));
        self.display_states.insert(selected, current.next());
        self.bump_revision();
        true
    }

    pub(in crate::interactive) fn toggle_selected_arguments(
        &mut self,
        transcript: &Transcript,
    ) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let Some(item) = transcript.item_for_block(selected) else {
            return false;
        };
        if !item.has_tool_arguments() {
            return false;
        }
        let current = self
            .tool_argument_states
            .get(&selected)
            .copied()
            .unwrap_or_else(|| default_tool_argument_state(item));
        self.tool_argument_states.insert(selected, current.next());
        self.bump_revision();
        true
    }

    pub(in crate::interactive) fn selected_has_tool_arguments(
        &self,
        transcript: &Transcript,
    ) -> bool {
        self.selected
            .and_then(|selected| transcript.item_for_block(selected))
            .is_some_and(TranscriptItem::has_tool_arguments)
    }

    pub(in crate::interactive) fn toggle_all(&mut self, transcript: &Transcript) -> bool {
        let foldable = transcript
            .view_entries()
            .filter(|(_, item)| item.foldable())
            .collect::<Vec<_>>();
        if foldable.is_empty() {
            return false;
        }
        let all_expanded = foldable.iter().all(|(id, item)| {
            let body_expanded = self
                .display_states
                .get(id)
                .copied()
                .unwrap_or_else(|| default_display_state(item))
                == TranscriptDisplayState::Expanded;
            let arguments_expanded = !item.has_tool_arguments()
                || self
                    .tool_argument_states
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| default_tool_argument_state(item))
                    == TranscriptDisplayState::Expanded;
            body_expanded && arguments_expanded
        });
        for (id, item) in foldable {
            if all_expanded {
                self.display_states.remove(&id);
                self.tool_argument_states.remove(&id);
            } else {
                self.display_states
                    .insert(id, TranscriptDisplayState::Expanded);
                if item.has_tool_arguments() {
                    self.tool_argument_states
                        .insert(id, TranscriptDisplayState::Expanded);
                }
            }
        }
        self.bump_revision();
        true
    }

    fn move_selection(&mut self, transcript: &Transcript, delta: isize) -> bool {
        let ids = transcript
            .view_entries()
            .filter(|(_, item)| item.selectable())
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return false;
        }
        let current = self
            .selected
            .and_then(|selected| ids.iter().position(|id| *id == selected))
            .unwrap_or(ids.len() - 1);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(ids.len() - 1)
        };
        if next == current && self.selected == Some(ids[next]) {
            return false;
        }
        self.selected = Some(ids[next]);
        self.bump_revision();
        true
    }

    fn select_boundary(&mut self, transcript: &Transcript, last: bool) -> bool {
        let selected = if last {
            transcript
                .view_entries()
                .rev()
                .find(|(_, item)| item.selectable())
                .map(|(id, _)| id)
        } else {
            transcript
                .view_entries()
                .find(|(_, item)| item.selectable())
                .map(|(id, _)| id)
        };
        let Some(selected) = selected else {
            return false;
        };
        if self.selected == Some(selected) {
            return false;
        }
        self.selected = Some(selected);
        self.bump_revision();
        true
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        *self.cached_snapshot.get_mut() = None;
    }
}

pub(in crate::interactive) fn default_display_state(
    item: &TranscriptItem,
) -> TranscriptDisplayState {
    match item {
        TranscriptItem::Assistant { thinking, .. } if !thinking.trim().is_empty() => {
            TranscriptDisplayState::Preview
        }
        TranscriptItem::Tool { is_error: true, .. } => TranscriptDisplayState::Expanded,
        TranscriptItem::Tool { .. } => TranscriptDisplayState::Preview,
        _ => TranscriptDisplayState::Expanded,
    }
}

fn default_tool_argument_state(item: &TranscriptItem) -> TranscriptDisplayState {
    match item {
        TranscriptItem::Tool { name, .. }
            if matches!(
                name.as_str(),
                "read" | "bash" | "grep" | "find" | "ls" | "write" | "edit" | "delegation"
            ) =>
        {
            TranscriptDisplayState::Collapsed
        }
        TranscriptItem::Tool { .. } => TranscriptDisplayState::Preview,
        _ => TranscriptDisplayState::Collapsed,
    }
}
