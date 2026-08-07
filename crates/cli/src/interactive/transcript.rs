use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

mod view_state;
#[cfg(test)]
mod view_state_tests;

pub(super) use view_state::{
    TranscriptBlockId, TranscriptDisplayState, TranscriptViewSnapshot, TranscriptViewState,
};

static NEXT_TRANSCRIPT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User {
        text: String,
    },
    Assistant {
        id: String,
        markdown: String,
        thinking: String,
        thinking_seconds: Option<f64>,
        done: bool,
    },
    Tool {
        call_id: String,
        name: String,
        args: serde_json::Value,
        result: Option<String>,
        is_error: bool,
    },
    Image {
        mime_type: String,
        data: String,
    },
    Error {
        text: String,
    },
    System {
        text: String,
    },
}

impl TranscriptItem {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User { text: text.into() }
    }

    pub fn assistant(id: impl Into<String>, markdown: impl Into<String>, done: bool) -> Self {
        Self::Assistant {
            id: id.into(),
            markdown: markdown.into(),
            thinking: String::new(),
            thinking_seconds: None,
            done,
        }
    }

    #[cfg(test)]
    pub fn error(text: impl Into<String>) -> Self {
        Self::Error { text: text.into() }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::System { text: text.into() }
    }

    pub(super) fn selectable(&self) -> bool {
        match self {
            Self::Assistant {
                markdown, thinking, ..
            } => !markdown.trim().is_empty() || !thinking.trim().is_empty(),
            Self::System { .. } => false,
            _ => true,
        }
    }

    pub(super) fn foldable(&self) -> bool {
        matches!(
            self,
            Self::Assistant { thinking, .. } if !thinking.trim().is_empty()
        ) || matches!(self, Self::Tool { .. })
    }

    fn has_tool_arguments(&self) -> bool {
        match self {
            Self::Tool { args, .. } => match args {
                serde_json::Value::Null => false,
                serde_json::Value::Object(object) => !object.is_empty(),
                serde_json::Value::Array(array) => !array.is_empty(),
                _ => true,
            },
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TranscriptRenderKey {
    pub(crate) transcript_id: u64,
    pub(crate) item_id: u64,
    pub(crate) item_revision: u64,
}

impl TranscriptRenderKey {
    pub(super) fn block_id(self) -> TranscriptBlockId {
        TranscriptBlockId {
            transcript_id: self.transcript_id,
            item_id: self.item_id,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TranscriptMutation {
    changed_indices: Vec<usize>,
}

impl TranscriptMutation {
    fn none() -> Self {
        Self::default()
    }

    fn single(index: usize) -> Self {
        Self {
            changed_indices: vec![index],
        }
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.changed_indices.extend(other.changed_indices);
    }

    pub(crate) fn changed_indices(&self) -> &[usize] {
        &self.changed_indices
    }
}

#[derive(Debug, Clone)]
struct TranscriptItemMeta {
    item_id: u64,
    revision: u64,
}

#[derive(Debug)]
pub struct Transcript {
    items: Vec<TranscriptItem>,
    item_meta: Vec<TranscriptItemMeta>,
    scroll_offset: usize,
    new_output_below: bool,
    content_revision: u64,
    revision: u64,
    cache_id: u64,
    next_item_id: u64,
    thinking_started_at: Option<Instant>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            item_meta: Vec::new(),
            scroll_offset: 0,
            new_output_below: false,
            content_revision: 0,
            revision: 0,
            cache_id: NEXT_TRANSCRIPT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            next_item_id: 0,
            thinking_started_at: None,
        }
    }
}

impl Clone for Transcript {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            item_meta: self.item_meta.clone(),
            scroll_offset: self.scroll_offset,
            new_output_below: self.new_output_below,
            content_revision: self.content_revision,
            revision: self.revision,
            cache_id: NEXT_TRANSCRIPT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            next_item_id: self.next_item_id,
            thinking_started_at: self.thinking_started_at,
        }
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: TranscriptItem) {
        self.push_with_index(item);
    }

    fn push_with_index(&mut self, item: TranscriptItem) -> usize {
        self.record_output_below();
        let item_id = self.next_item_id;
        self.next_item_id = self.next_item_id.wrapping_add(1);
        let index = self.items.len();
        self.items.push(item);
        self.item_meta.push(TranscriptItemMeta {
            item_id,
            revision: 0,
        });
        self.bump_content_revision();
        index
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    pub(crate) fn retain_recent(&mut self, max_items: usize) {
        if self.items.len() <= max_items {
            return;
        }
        let remove = self.items.len().saturating_sub(max_items);
        self.items.drain(..remove);
        self.item_meta.drain(..remove);
        self.scroll_offset = 0;
        self.new_output_below = false;
        self.bump_content_revision();
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn content_revision(&self) -> u64 {
        self.content_revision
    }

    pub(crate) fn render_cache_id(&self) -> u64 {
        self.cache_id
    }

    pub(crate) fn render_entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = (TranscriptRenderKey, &TranscriptItem)> {
        debug_assert_eq!(self.items.len(), self.item_meta.len());
        let transcript_id = self.cache_id;
        self.items
            .iter()
            .zip(self.item_meta.iter())
            .map(move |(item, meta)| {
                (
                    TranscriptRenderKey {
                        transcript_id,
                        item_id: meta.item_id,
                        item_revision: meta.revision,
                    },
                    item,
                )
            })
    }

    fn view_entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = (TranscriptBlockId, &TranscriptItem)> {
        self.render_entries()
            .map(|(render_key, item)| (render_key.block_id(), item))
    }

    pub(super) fn item_for_block(&self, block_id: TranscriptBlockId) -> Option<&TranscriptItem> {
        if block_id.transcript_id != self.cache_id {
            return None;
        }
        self.item_meta
            .iter()
            .position(|meta| meta.item_id == block_id.item_id)
            .and_then(|index| self.items.get(index))
    }

    pub(crate) fn render_entry_at(
        &self,
        index: usize,
    ) -> Option<(TranscriptRenderKey, &TranscriptItem)> {
        debug_assert_eq!(self.items.len(), self.item_meta.len());
        let item = self.items.get(index)?;
        let meta = self.item_meta.get(index)?;
        Some((
            TranscriptRenderKey {
                transcript_id: self.cache_id,
                item_id: meta.item_id,
                item_revision: meta.revision,
            },
            item,
        ))
    }

    pub fn scroll_page_up(&mut self, rows: usize) {
        let previous = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_add(rows);
        if self.scroll_offset != previous {
            self.bump_revision();
        }
    }

    pub fn scroll_page_down(&mut self, rows: usize) {
        let previous_offset = self.scroll_offset;
        let previous_new_output_below = self.new_output_below;
        self.scroll_offset = self.scroll_offset.saturating_sub(rows);
        if self.scroll_offset == 0 {
            self.new_output_below = false;
        }
        if self.scroll_offset != previous_offset
            || self.new_output_below != previous_new_output_below
        {
            self.bump_revision();
        }
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn has_new_output_below(&self) -> bool {
        self.new_output_below
    }

    pub(crate) fn preserve_scrolled_view_after_hidden_change(
        &mut self,
        previous_scroll_offset: usize,
        row_delta_below_anchor: isize,
    ) {
        if previous_scroll_offset == 0 {
            return;
        }
        let previous_offset = self.scroll_offset;
        let previous_new_output_below = self.new_output_below;
        self.scroll_offset = add_signed_rows(previous_scroll_offset, row_delta_below_anchor);
        self.new_output_below = true;
        if self.scroll_offset != previous_offset
            || self.new_output_below != previous_new_output_below
        {
            self.bump_revision();
        }
    }

    pub(crate) fn preserve_scrolled_view_after_row_change(
        &mut self,
        previous_scroll_offset: usize,
        previous_total_rows: usize,
        current_total_rows: usize,
    ) {
        if previous_scroll_offset == 0 {
            return;
        }
        let previous = self.scroll_offset;
        self.scroll_offset = if current_total_rows >= previous_total_rows {
            previous_scroll_offset.saturating_add(current_total_rows - previous_total_rows)
        } else {
            previous_scroll_offset.saturating_sub(previous_total_rows - current_total_rows)
        };
        if self.scroll_offset != previous {
            self.bump_revision();
        }
    }

    pub(crate) fn ensure_row_range_visible(
        &mut self,
        total_rows: usize,
        row_start: usize,
        row_end: usize,
        viewport_height: usize,
    ) {
        if viewport_height == 0 || row_start >= row_end {
            return;
        }
        let max_offset = total_rows.saturating_sub(viewport_height);
        let current_offset = self.scroll_offset.min(max_offset);
        let viewport_end = total_rows.saturating_sub(current_offset);
        let viewport_start = viewport_end.saturating_sub(viewport_height);
        let next_offset =
            if row_end.saturating_sub(row_start) >= viewport_height || row_start < viewport_start {
                total_rows.saturating_sub(row_start.saturating_add(viewport_height))
            } else if row_end > viewport_end {
                total_rows.saturating_sub(row_end)
            } else {
                current_offset
            }
            .min(max_offset);
        if next_offset != self.scroll_offset {
            self.scroll_offset = next_offset;
            if self.scroll_offset == 0 {
                self.new_output_below = false;
            }
            self.bump_revision();
        }
    }

    #[cfg(test)]
    pub fn apply_event(&mut self, event: UiEvent) {
        self.apply_event_with_mutation(event);
    }

    pub(crate) fn apply_event_with_mutation(&mut self, event: UiEvent) -> TranscriptMutation {
        match event {
            UiEvent::TurnStarted => self.close_open_assistant(),
            UiEvent::AssistantDelta { text } => self.append_assistant_delta(&text),
            UiEvent::ThinkingDelta { text } => self.append_assistant_thinking(&text),
            UiEvent::AssistantDone => self.mark_assistant_done(),
            UiEvent::AssistantImages { images } => {
                let mut mutation = TranscriptMutation::none();
                for image in images {
                    mutation.extend(TranscriptMutation::single(self.push_with_index(
                        TranscriptItem::Image {
                            mime_type: image.mime_type,
                            data: image.data,
                        },
                    )));
                }
                mutation
            }
            UiEvent::ToolStarted {
                call_id,
                name,
                args,
            } => {
                let mut mutation = self.close_open_assistant();
                mutation.extend(TranscriptMutation::single(self.push_with_index(
                    TranscriptItem::Tool {
                        call_id,
                        name,
                        args,
                        result: None,
                        is_error: false,
                    },
                )));
                mutation
            }
            UiEvent::ToolFinished {
                call_id,
                result,
                is_error,
            } => self.finish_tool(&call_id, result, is_error),
            UiEvent::ToolUpdated { call_id, result } => self.update_tool(&call_id, result),
            UiEvent::AgentError { error } => {
                let mut mutation = self.close_open_assistant();
                mutation.extend(TranscriptMutation::single(
                    self.push_with_index(TranscriptItem::Error { text: error }),
                ));
                mutation
            }
            UiEvent::SystemNotice { text } => {
                let mut mutation = self.close_open_assistant();
                mutation.extend(TranscriptMutation::single(
                    self.push_with_index(TranscriptItem::System { text }),
                ));
                mutation
            }
            UiEvent::DelegationBlock {
                call_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
                is_error,
            } => self.upsert_delegation_block(
                call_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
                is_error,
            ),
            UiEvent::CompactionNotice { summary } => {
                TranscriptMutation::single(self.push_with_index(TranscriptItem::Assistant {
                    id: format!("compaction_{}", self.items.len()),
                    markdown: summary,
                    thinking: String::new(),
                    thinking_seconds: None,
                    done: true,
                }))
            }
            UiEvent::UsageUpdate { .. } => TranscriptMutation::none(),
            UiEvent::ToolAuthorizationRequired { .. }
            | UiEvent::ToolAuthorizationResolved { .. }
            | UiEvent::DelegationConfirmationRequired { .. }
            | UiEvent::DelegationConfirmationResolved { .. } => TranscriptMutation::none(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_delegation_block(
        &mut self,
        call_id: String,
        target_kind: String,
        target_id: String,
        task: String,
        status: String,
        child_operation_id: Option<String>,
        summary: Option<String>,
        is_error: bool,
    ) -> TranscriptMutation {
        let args = delegation_args(target_kind, target_id, task, status, child_operation_id);
        if let Some(index) = self.items.iter().position(|item| {
            matches!(item, TranscriptItem::Tool { call_id: existing, .. } if existing == &call_id)
        }) {
            let TranscriptItem::Tool {
                name,
                args: existing_args,
                result,
                is_error: existing_error,
                ..
            } = &mut self.items[index]
            else {
                unreachable!("position matched tool item");
            };
            *name = "delegation".to_string();
            *existing_args = args;
            if summary.is_some() {
                *result = summary;
            }
            *existing_error = is_error;
            self.record_output_below();
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }

        let mut mutation = self.close_open_assistant();
        mutation.extend(TranscriptMutation::single(self.push_with_index(
            TranscriptItem::Tool {
                call_id,
                name: "delegation".to_string(),
                args,
                result: summary,
                is_error,
            },
        )));
        mutation
    }

    fn append_assistant_delta(&mut self, text: &str) -> TranscriptMutation {
        let was_scrolled = self.scroll_offset > 0;
        if let Some(index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, TranscriptItem::Assistant { done: false, .. }))
        {
            let TranscriptItem::Assistant { markdown, done, .. } = &mut self.items[index] else {
                unreachable!("rposition matched unfinished assistant");
            };
            if was_scrolled {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                self.new_output_below = true;
            } else {
                self.scroll_offset = 0;
                self.new_output_below = false;
            }
            markdown.push_str(text);
            *done = false;
            // Body output marks the end of thinking.
            self.seal_assistant_thinking(index);
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }

        TranscriptMutation::single(self.push_with_index(TranscriptItem::Assistant {
            id: format!("assistant_{}", self.items.len()),
            markdown: text.to_string(),
            thinking: String::new(),
            thinking_seconds: None,
            done: false,
        }))
    }

    fn append_assistant_thinking(&mut self, text: &str) -> TranscriptMutation {
        let was_scrolled = self.scroll_offset > 0;
        if let Some(index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, TranscriptItem::Assistant { done: false, .. }))
        {
            let TranscriptItem::Assistant { thinking, done, .. } = &mut self.items[index] else {
                unreachable!("rposition matched unfinished assistant");
            };
            if was_scrolled {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                self.new_output_below = true;
            } else {
                self.scroll_offset = 0;
                self.new_output_below = false;
            }
            thinking.push_str(text);
            *done = false;
            if self.thinking_started_at.is_none() {
                self.thinking_started_at = Some(Instant::now());
            }
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }

        self.thinking_started_at = Some(Instant::now());
        TranscriptMutation::single(self.push_with_index(TranscriptItem::Assistant {
            id: format!("assistant_{}", self.items.len()),
            markdown: String::new(),
            thinking: text.to_string(),
            thinking_seconds: None,
            done: false,
        }))
    }

    fn mark_assistant_done(&mut self) -> TranscriptMutation {
        if let Some(index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, TranscriptItem::Assistant { done: false, .. }))
        {
            let TranscriptItem::Assistant { done, .. } = &mut self.items[index] else {
                unreachable!("rposition matched unfinished assistant");
            };
            *done = true;
            self.seal_assistant_thinking(index);
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }

        TranscriptMutation::none()
    }

    fn close_open_assistant(&mut self) -> TranscriptMutation {
        let Some(index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, TranscriptItem::Assistant { done: false, .. }))
        else {
            return TranscriptMutation::none();
        };
        let TranscriptItem::Assistant { done, .. } = &mut self.items[index] else {
            unreachable!("rposition matched unfinished assistant");
        };
        *done = true;
        self.seal_assistant_thinking(index);
        self.bump_item_revision(index);
        self.bump_content_revision();
        TranscriptMutation::single(index)
    }

    /// Record how long the assistant thought once its output moves on
    /// (body text, tool use, or completion). Only sealed once per block.
    fn seal_assistant_thinking(&mut self, index: usize) {
        let Some(started_at) = self.thinking_started_at.take() else {
            return;
        };
        let TranscriptItem::Assistant {
            thinking,
            thinking_seconds,
            ..
        } = &mut self.items[index]
        else {
            return;
        };
        if !thinking.trim().is_empty() && thinking_seconds.is_none() {
            *thinking_seconds = Some(started_at.elapsed().as_secs_f64());
        }
    }

    fn finish_tool(&mut self, call_id: &str, result: String, is_error: bool) -> TranscriptMutation {
        if let Some(index) = self.items.iter().position(|item| {
            matches!(item, TranscriptItem::Tool { call_id: existing, .. } if existing == call_id)
        }) {
            let TranscriptItem::Tool {
                result: existing,
                is_error: existing_error,
                ..
            } = &mut self.items[index]
            else {
                unreachable!("position matched tool item");
            };
            *existing = Some(result);
            *existing_error = is_error;
            self.record_output_below();
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }
        TranscriptMutation::none()
    }

    fn update_tool(&mut self, call_id: &str, result: String) -> TranscriptMutation {
        if let Some(index) = self.items.iter().position(|item| {
            matches!(item, TranscriptItem::Tool { call_id: existing, .. } if existing == call_id)
        }) {
            let TranscriptItem::Tool {
                result: existing, ..
            } = &mut self.items[index]
            else {
                unreachable!("position matched tool item");
            };
            *existing = Some(result);
            self.record_output_below();
            self.bump_item_revision(index);
            self.bump_content_revision();
            return TranscriptMutation::single(index);
        }
        TranscriptMutation::none()
    }

    fn record_output_below(&mut self) {
        if self.scroll_offset == 0 {
            self.scroll_offset = 0;
            self.new_output_below = false;
        } else {
            self.scroll_offset = self.scroll_offset.saturating_add(1);
            self.new_output_below = true;
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn bump_content_revision(&mut self) {
        self.content_revision = self.content_revision.wrapping_add(1);
        self.bump_revision();
    }

    fn bump_item_revision(&mut self, index: usize) {
        if let Some(meta) = self.item_meta.get_mut(index) {
            meta.revision = meta.revision.wrapping_add(1);
        }
    }
}

fn add_signed_rows(value: usize, delta: isize) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

fn delegation_args(
    target_kind: String,
    target_id: String,
    task: String,
    status: String,
    child_operation_id: Option<String>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "targetKind": target_kind,
        "targetId": target_id,
        "task": task,
        "status": status,
    });
    if let Some(child_operation_id) = child_operation_id
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "childOperationId".to_string(),
            serde_json::Value::String(child_operation_id),
        );
    }
    value
}

use super::UiEvent;
