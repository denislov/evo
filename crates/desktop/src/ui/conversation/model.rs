//! Bounded conversation transcript identity and projection state.
//!
//! These reducers remain independent of GPUI. The renderer may virtualize the
//! resulting blocks without owning product transcript truth.

use std::collections::VecDeque;
use std::sync::Arc;

use coding_agent::api::view::CodingAgentTranscriptSnapshot;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::copy::conversation_copy_text;

mod blocks;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use self::blocks::compact_duration;
pub(crate) use self::blocks::{block_from_product, conversation_block_revision};

pub const MAX_TRANSCRIPT_BLOCKS: usize = 10_000;
pub const MAX_TRANSCRIPT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BLOCK_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_THINKING_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
/// Title prefix shared with the copy path so delegation titles cannot drift.
pub const DELEGATION_TITLE_PREFIX: &str = "Delegation · ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationBlockKind {
    User,
    Assistant,
    Tool,
    Delegation,
    CompactionSummary,
    BranchSummary,
    Diagnostic,
}

impl ConversationBlockKind {
    const fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Delegation => "delegation",
            Self::CompactionSummary => "compaction",
            Self::BranchSummary => "branch",
            Self::Diagnostic => "diagnostic",
        }
    }
}

/// Durable lifecycle state of a delegation, parsed from the delegate tool's
/// status string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationStatus {
    Requested,
    Running,
    Completed,
    Failed,
    Rejected,
    Cancelled,
    ConfirmationRequired,
    Unknown,
}

impl DelegationStatus {
    pub fn parse(status: &str) -> Self {
        match status {
            "requested" => Self::Requested,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            "confirmation_required" => Self::ConfirmationRequired,
            _ => Self::Unknown,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Requested => "Requested",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Rejected => "Rejected",
            Self::Cancelled => "Cancelled",
            Self::ConfirmationRequired => "Awaiting approval",
            Self::Unknown => "Delegated",
        }
    }
}

/// Display metadata for a delegation block: which profile received the task
/// and how the delegation resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationMeta {
    pub target_id: String,
    pub status: DelegationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationItemKind {
    Durable(ConversationBlockKind),
    Submitted,
    LiveMessage,
    LiveTool,
}

impl ConversationItemKind {
    const fn key(self) -> &'static str {
        match self {
            Self::Durable(kind) => kind.key(),
            Self::Submitted => "submitted",
            Self::LiveMessage => "live-message",
            Self::LiveTool => "live-tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationItemKey {
    session_id: Arc<str>,
    kind: ConversationItemKind,
    row_id: Arc<str>,
    stable_id: Arc<str>,
}

impl ConversationItemKey {
    pub fn new(session_id: &str, kind: ConversationItemKind, row_id: &str) -> Self {
        Self {
            session_id: Arc::from(session_id),
            kind,
            row_id: Arc::from(row_id),
            stable_id: Arc::from(format!(
                "{}:{session_id}:{}:{}:{row_id}",
                session_id.len(),
                kind.key(),
                row_id.len()
            )),
        }
    }

    pub fn row_id(&self) -> &str {
        &self.row_id
    }

    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub fn stable_id_arc(&self) -> Arc<str> {
        Arc::clone(&self.stable_id)
    }

    /// The GPUI element id backing this row's Markdown view.
    ///
    /// Deliberately free of the source revision. `TextView` keys its parsed
    /// `TextViewState` off the element id, and its `set_text` already
    /// short-circuits when the text is unchanged, so a per-revision id threw away
    /// and rebuilt that state on every streaming delta — losing the text
    /// selection with it. Streaming and completed content deliberately share
    /// the same key, so one `TextViewState` survives from the first chunk
    /// through completion.
    pub(super) fn markdown_state_key(&self, detail: bool) -> Arc<str> {
        let namespace = if detail {
            "transcript-detail-markdown"
        } else {
            "transcript-markdown"
        };
        Arc::from(format!(
            "{namespace}:{}:{}:{}",
            self.session_id.len(),
            self.session_id,
            self.row_id
        ))
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.session_id.len() + self.row_id.len() + self.stable_id.len()
    }
}

/// Turn-level display metadata attached to the turn's final assistant row:
/// which model answered and how long the whole turn (submit to completion,
/// tool calls included) took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnMeta {
    pub model: String,
    pub duration_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBlock {
    pub id: String,
    /// Stable content revision computed once while hydrating product state.
    pub source_revision: u64,
    pub kind: ConversationBlockKind,
    pub title: String,
    pub text: String,
    pub detail: String,
    pub done: bool,
    pub is_error: bool,
    pub image_count: usize,
    pub reasoning_duration_millis: Option<u64>,
    pub truncated: bool,
    /// Model that actually produced this assistant message (`response_model`
    /// when the provider reported one, otherwise the requested model).
    pub model: Option<String>,
    /// Wall-clock submit time (RFC 3339) of the turn this user row opened.
    pub started_at: Option<String>,
    /// Wall-clock completion time (RFC 3339) of this assistant message.
    pub completed_at: Option<String>,
    /// Turn summary attached to the turn's final assistant row; `None` for
    /// interior rows and rows outside a completed turn.
    pub turn: Option<TurnMeta>,
    /// Delegation target and lifecycle state; `None` for every other kind.
    pub delegation: Option<DelegationMeta>,
}

impl ConversationBlock {
    pub fn copy_text(&self) -> String {
        conversation_copy_text(&self.text, &self.detail)
    }

    fn retained_bytes(&self) -> usize {
        self.id.len()
            + self.title.len()
            + self.text.len()
            + self.detail.len()
            + self.model.as_ref().map_or(0, String::len)
            + self.started_at.as_ref().map_or(0, String::len)
            + self.completed_at.as_ref().map_or(0, String::len)
            + self.turn.as_ref().map_or(0, |turn| turn.model.len())
            + self
                .delegation
                .as_ref()
                .map_or(0, |meta| meta.target_id.len())
    }

    fn refresh_source_revision(&mut self) {
        self.source_revision = conversation_block_revision(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationProjection {
    pub session_id: String,
    pub active_leaf_id: Option<String>,
    blocks: VecDeque<ConversationBlock>,
    omitted_blocks: usize,
    retained_bytes: usize,
}

impl ConversationProjection {
    pub fn hydrate(snapshot: CodingAgentTranscriptSnapshot) -> Self {
        let omitted_items = snapshot.omitted_items;
        let mut projection = Self {
            session_id: snapshot.session_id,
            active_leaf_id: snapshot.active_leaf_id,
            blocks: VecDeque::with_capacity(snapshot.items.len().min(MAX_TRANSCRIPT_BLOCKS)),
            omitted_blocks: omitted_items,
            retained_bytes: 0,
        };
        for (index, item) in snapshot.items.into_iter().enumerate() {
            projection.push_bounded(block_from_product(index, item));
        }
        projection.refresh_turn_metadata();
        projection
    }

    pub fn blocks(&self) -> &VecDeque<ConversationBlock> {
        &self.blocks
    }

    pub const fn omitted_blocks(&self) -> usize {
        self.omitted_blocks
    }

    #[cfg(test)]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn block(&self, id: &str) -> Option<&ConversationBlock> {
        self.blocks.iter().find(|block| block.id == id)
    }

    fn push_bounded(&mut self, block: ConversationBlock) {
        if block.kind == ConversationBlockKind::Diagnostic {
            let merged_bytes = self.blocks.back_mut().and_then(|previous| {
                let equivalent = previous.kind == ConversationBlockKind::Diagnostic
                    && diagnostic_equivalence_key(&previous.text)
                        == diagnostic_equivalence_key(&block.text);
                if !equivalent {
                    return None;
                }

                let before = previous.retained_bytes();
                previous.title = next_diagnostic_title(&previous.title);
                previous.truncated |= block.truncated;
                previous.refresh_source_revision();
                Some((before, previous.retained_bytes()))
            });
            if let Some((before, after)) = merged_bytes {
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(before)
                    .saturating_add(after);
                return;
            }
        }

        self.retained_bytes = self.retained_bytes.saturating_add(block.retained_bytes());
        self.blocks.push_back(block);
        while self.blocks.len() > MAX_TRANSCRIPT_BLOCKS
            || self.retained_bytes > MAX_TRANSCRIPT_BYTES
        {
            let Some(evicted) = self.blocks.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(evicted.retained_bytes());
            self.omitted_blocks = self.omitted_blocks.saturating_add(1);
        }
    }

    /// Attach the turn summary (model + whole-turn duration) to the final
    /// assistant row of every completed turn: from the user's submit time to
    /// the last assistant message's completion, tool calls included.
    fn refresh_turn_metadata(&mut self) {
        let mut turn_started_at: Option<String> = None;
        // Index of the last assistant row in the open turn; its `turn` field
        // receives the summary once the turn closes or the transcript ends.
        let mut last_assistant_index: Option<usize> = None;
        let mut last_model: Option<String> = None;
        let mut last_completed_at: Option<String> = None;
        let mut finalized = Vec::<PendingTurnFinalize>::new();
        for index in 0..self.blocks.len() {
            match self.blocks[index].kind {
                ConversationBlockKind::User => {
                    if let Some(assistant_index) = last_assistant_index.take() {
                        finalized.push(PendingTurnFinalize {
                            assistant_index,
                            started_at: turn_started_at.clone(),
                            completed_at: last_completed_at.take(),
                            model: last_model.take(),
                        });
                    }
                    turn_started_at = self.blocks[index].started_at.clone();
                }
                ConversationBlockKind::Assistant => {
                    last_assistant_index = Some(index);
                    if let Some(model) = &self.blocks[index].model {
                        last_model = Some(model.clone());
                    }
                    if let Some(completed_at) = &self.blocks[index].completed_at {
                        last_completed_at = Some(completed_at.clone());
                    }
                }
                _ => {}
            }
        }
        if let Some(assistant_index) = last_assistant_index {
            finalized.push(PendingTurnFinalize {
                assistant_index,
                started_at: turn_started_at,
                completed_at: last_completed_at,
                model: last_model,
            });
        }
        for pending in finalized {
            self.finalize_turn(
                pending.assistant_index,
                &pending.started_at,
                pending.completed_at,
                pending.model,
            );
        }
    }

    fn finalize_turn(
        &mut self,
        assistant_index: usize,
        turn_started_at: &Option<String>,
        completed_at: Option<String>,
        model: Option<String>,
    ) {
        let Some(model) = model else {
            return;
        };
        let duration_millis = match (&turn_started_at, &completed_at) {
            (Some(started_at), Some(completed_at)) => {
                rfc3339_elapsed_millis(started_at, completed_at)
            }
            _ => None,
        };
        let Some(block) = self.blocks.get_mut(assistant_index) else {
            return;
        };
        block.turn = Some(TurnMeta {
            model,
            duration_millis,
        });
        block.refresh_source_revision();
    }
}

/// Finalize work for one closed turn, collected while scanning the transcript.
struct PendingTurnFinalize {
    assistant_index: usize,
    started_at: Option<String>,
    completed_at: Option<String>,
    model: Option<String>,
}

/// Whole-turn wall-clock duration in milliseconds between two RFC 3339
/// timestamps; `None` when either side fails to parse.
fn rfc3339_elapsed_millis(started_at: &str, completed_at: &str) -> Option<u64> {
    let started_at = OffsetDateTime::parse(started_at, &Rfc3339).ok()?;
    let completed_at = OffsetDateTime::parse(completed_at, &Rfc3339).ok()?;
    u64::try_from((completed_at - started_at).whole_milliseconds()).ok()
}

fn diagnostic_equivalence_key(message: &str) -> &str {
    message
        .strip_prefix("provider error: ")
        .unwrap_or(message)
        .trim()
}

fn next_diagnostic_title(title: &str) -> String {
    let count = title
        .strip_prefix("Diagnostic · ")
        .and_then(|value| value.strip_suffix(" related events"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .saturating_add(1);
    format!("Diagnostic · {count} related events")
}
