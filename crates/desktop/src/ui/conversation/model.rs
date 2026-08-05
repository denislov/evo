//! Bounded conversation transcript identity and projection state.
//!
//! These reducers remain independent of GPUI. The renderer may virtualize the
//! resulting blocks without owning product transcript truth.

use std::collections::VecDeque;
use std::sync::Arc;

use coding_agent::api::view::{CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::copy::{conversation_copy_text, truncate_bytes};

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

fn block_from_product(index: usize, item: CodingAgentSessionTranscriptItem) -> ConversationBlock {
    let reasoning_duration_millis = match &item {
        CodingAgentSessionTranscriptItem::Assistant {
            reasoning_duration_millis,
            ..
        } => *reasoning_duration_millis,
        _ => None,
    };
    let model = match &item {
        CodingAgentSessionTranscriptItem::Assistant { model_id, .. } => model_id.clone(),
        _ => None,
    };
    let started_at = match &item {
        CodingAgentSessionTranscriptItem::User { started_at, .. } => started_at.clone(),
        _ => None,
    };
    let completed_at = match &item {
        CodingAgentSessionTranscriptItem::Assistant { completed_at, .. } => completed_at.clone(),
        _ => None,
    };
    let (kind, source_id, title, text, detail, done, is_error, image_count, truncated, delegation) =
        match item {
            CodingAgentSessionTranscriptItem::User { text, .. } => {
                let (text, truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::User,
                    String::new(),
                    "You".into(),
                    text,
                    String::new(),
                    true,
                    false,
                    0,
                    truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Assistant {
                id,
                text,
                thinking,
                images,
                done,
                ..
            } => {
                let (text, text_truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
                let (thinking, thinking_truncated) =
                    truncate_bytes(thinking, MAX_THINKING_TEXT_BYTES);
                (
                    ConversationBlockKind::Assistant,
                    id,
                    "Assistant".into(),
                    text,
                    thinking,
                    done,
                    false,
                    images.len(),
                    text_truncated || thinking_truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Tool {
                call_id,
                name,
                args,
                result,
                is_error,
                duration_millis,
            } => {
                let arguments = serde_json::to_string_pretty(&args)
                    .unwrap_or_else(|_| "<invalid tool arguments>".into());
                let (arguments, args_truncated) =
                    truncate_bytes(arguments, MAX_TOOL_ARGUMENT_BYTES);
                let (result, result_truncated) =
                    truncate_bytes(result.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Tool,
                    call_id,
                    tool_title(&name, duration_millis),
                    result,
                    arguments,
                    true,
                    is_error,
                    0,
                    args_truncated || result_truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Delegation {
                tool_call_id,
                target_kind,
                target_id,
                task,
                status,
                summary,
                ..
            } => {
                let (task, task_truncated) = truncate_bytes(task, MAX_BLOCK_TEXT_BYTES);
                let (summary, summary_truncated) =
                    truncate_bytes(summary.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Delegation,
                    tool_call_id,
                    format!("{DELEGATION_TITLE_PREFIX}{target_kind:?}"),
                    task,
                    summary,
                    true,
                    false,
                    0,
                    task_truncated || summary_truncated,
                    Some(DelegationMeta {
                        target_id: target_id.to_string(),
                        status: DelegationStatus::parse(&status),
                    }),
                )
            }
            CodingAgentSessionTranscriptItem::CompactionSummary { summary } => summary_block(
                ConversationBlockKind::CompactionSummary,
                "Compaction",
                summary,
            ),
            CodingAgentSessionTranscriptItem::BranchSummary { summary } => summary_block(
                ConversationBlockKind::BranchSummary,
                "Branch summary",
                summary,
            ),
            CodingAgentSessionTranscriptItem::Diagnostic { message } => {
                let (message, truncated) = truncate_bytes(message, MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Diagnostic,
                    String::new(),
                    "Diagnostic".into(),
                    message,
                    String::new(),
                    true,
                    true,
                    0,
                    truncated,
                    None,
                )
            }
        };
    let id = if source_id.is_empty() {
        format!("{index:08}:{}", kind.key())
    } else {
        format!("{}:{source_id}", kind.key())
    };
    let mut block = ConversationBlock {
        id,
        source_revision: 0,
        kind,
        title,
        text,
        detail,
        done,
        is_error,
        image_count,
        reasoning_duration_millis,
        truncated,
        model,
        started_at,
        completed_at,
        turn: None,
        delegation,
    };
    block.refresh_source_revision();
    block
}

fn tool_title(name: &str, duration_millis: Option<u64>) -> String {
    match duration_millis {
        Some(duration_millis) => {
            format!("Tool · {name} · {}", compact_duration(duration_millis))
        }
        None => format!("Tool · {name}"),
    }
}

/// Formats an authoritative lifecycle duration using stable compact units.
pub fn compact_duration(duration_millis: u64) -> String {
    if duration_millis < 1_000 {
        return format!("{duration_millis} ms");
    }
    if duration_millis < 60_000 {
        let rounded_tenths = duration_millis.saturating_add(50) / 100;
        if rounded_tenths < 600 {
            return format!("{}.{:01} s", rounded_tenths / 10, rounded_tenths % 10);
        }
    }
    let rounded_seconds = duration_millis.saturating_add(500) / 1_000;
    format!("{}m {:02}s", rounded_seconds / 60, rounded_seconds % 60)
}

fn conversation_block_revision(block: &ConversationBlock) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let mut hash = FNV_OFFSET;
    for value in [
        block.id.as_bytes(),
        block.kind.key().as_bytes(),
        block.title.as_bytes(),
        block.text.as_bytes(),
        block.detail.as_bytes(),
    ] {
        hash = update(hash, &(value.len() as u64).to_le_bytes());
        hash = update(hash, value);
    }
    hash = update(hash, &[u8::from(block.done)]);
    hash = update(hash, &[u8::from(block.is_error)]);
    hash = update(hash, &(block.image_count as u64).to_le_bytes());
    hash = update(
        hash,
        &block
            .reasoning_duration_millis
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    if let Some(model) = &block.model {
        hash = update(hash, &(model.len() as u64).to_le_bytes());
        hash = update(hash, model.as_bytes());
    }
    if let Some(started_at) = &block.started_at {
        hash = update(hash, &(started_at.len() as u64).to_le_bytes());
        hash = update(hash, started_at.as_bytes());
    }
    if let Some(completed_at) = &block.completed_at {
        hash = update(hash, &(completed_at.len() as u64).to_le_bytes());
        hash = update(hash, completed_at.as_bytes());
    }
    if let Some(turn) = &block.turn {
        hash = update(hash, &(turn.model.len() as u64).to_le_bytes());
        hash = update(hash, turn.model.as_bytes());
        hash = update(
            hash,
            &turn.duration_millis.unwrap_or(u64::MAX).to_le_bytes(),
        );
    }
    // A status-only transition (running -> cancelled, unchanged task and
    // summary) must still invalidate the render cache, or the header would
    // keep showing the stale status.
    if let Some(delegation) = &block.delegation {
        hash = update(hash, &(delegation.target_id.len() as u64).to_le_bytes());
        hash = update(hash, delegation.target_id.as_bytes());
        hash = update(hash, &[delegation.status as u8]);
    }
    update(hash, &[u8::from(block.truncated)])
}

fn summary_block(
    kind: ConversationBlockKind,
    title: &str,
    summary: String,
) -> (
    ConversationBlockKind,
    String,
    String,
    String,
    String,
    bool,
    bool,
    usize,
    bool,
    Option<DelegationMeta>,
) {
    let (summary, truncated) = truncate_bytes(summary, MAX_BLOCK_TEXT_BYTES);
    (
        kind,
        String::new(),
        title.into(),
        summary,
        String::new(),
        true,
        false,
        0,
        truncated,
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use coding_agent::api::view::{
        CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot,
    };

    use super::*;
    use crate::ui::conversation::markdown::bounded_markdown_preview;
    use crate::ui::conversation::{
        ComposerState, ConversationRowRenderCache, ConversationRowRenderSource,
        ConversationViewport,
    };

    fn transcript(items: Vec<CodingAgentSessionTranscriptItem>) -> CodingAgentTranscriptSnapshot {
        CodingAgentTranscriptSnapshot::new("session-1", Some("leaf-1".into()), items)
    }

    fn user(index: usize) -> CodingAgentSessionTranscriptItem {
        CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}"),
            started_at: None,
        }
    }

    #[test]
    fn delegation_status_parses_the_tool_vocabulary() {
        for (raw, expected, label) in [
            ("requested", DelegationStatus::Requested, "Requested"),
            ("running", DelegationStatus::Running, "Running"),
            ("completed", DelegationStatus::Completed, "Completed"),
            ("failed", DelegationStatus::Failed, "Failed"),
            ("rejected", DelegationStatus::Rejected, "Rejected"),
            ("cancelled", DelegationStatus::Cancelled, "Cancelled"),
            (
                "confirmation_required",
                DelegationStatus::ConfirmationRequired,
                "Awaiting approval",
            ),
        ] {
            let parsed = DelegationStatus::parse(raw);
            assert_eq!(parsed, expected, "{raw}");
            assert_eq!(parsed.label(), label, "{raw}");
        }
        assert_eq!(
            DelegationStatus::parse("mystery-status"),
            DelegationStatus::Unknown
        );
    }

    #[test]
    fn delegation_block_carries_target_and_status_metadata() {
        use coding_agent::api::view::{ProfileId, ProfileKind};
        let snapshot = transcript(vec![CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id: "call-delegation".into(),
            requesting_profile_id: ProfileId::new("sa-main").unwrap(),
            target_kind: ProfileKind::Agent,
            target_id: ProfileId::new("sa_plan").unwrap(),
            task: "Implement the auth flow\nsecond line".into(),
            status: "running".into(),
            child_operation_id: Some("op-1".into()),
            summary: None,
        }]);
        let block = ConversationProjection::hydrate(snapshot).blocks()[0].clone();
        assert_eq!(block.title, "Delegation · Agent");
        assert_eq!(block.text, "Implement the auth flow\nsecond line");
        // No summary yet: the detail stays empty instead of falling back to
        // the raw status string, which the header shows on its own.
        assert_eq!(block.detail, "");
        let meta = block.delegation.expect("delegation metadata");
        assert_eq!(meta.target_id, "sa_plan");
        assert_eq!(meta.status, DelegationStatus::Running);
        assert!(block.done);
        assert!(!block.is_error);
    }

    #[test]
    fn delegation_status_transitions_bump_the_block_revision() {
        use coding_agent::api::view::{ProfileId, ProfileKind};
        fn delegation_block(status: &str) -> ConversationBlock {
            let snapshot = transcript(vec![CodingAgentSessionTranscriptItem::Delegation {
                tool_call_id: "call-delegation".into(),
                requesting_profile_id: ProfileId::new("sa-main").unwrap(),
                target_kind: ProfileKind::Agent,
                target_id: ProfileId::new("sa_plan").unwrap(),
                task: "Implement the auth flow".into(),
                status: status.into(),
                child_operation_id: None,
                summary: None,
            }]);
            ConversationProjection::hydrate(snapshot).blocks()[0].clone()
        }
        // Same task and (empty) summary, only the status changes: the render
        // cache must see a new revision so the header chip re-renders.
        assert_ne!(
            delegation_block("running").source_revision,
            delegation_block("completed").source_revision
        );
        // Unrelated statuses stay stable across identical inputs.
        assert_eq!(
            delegation_block("running").source_revision,
            delegation_block("running").source_revision
        );
    }

    #[test]
    fn adjacent_equivalent_diagnostics_coalesce_for_presentation() {
        let message = "invalid terminal tool-call name";
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: format!("provider error: {message}"),
            },
            user(1),
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
        ]));

        assert_eq!(projection.blocks().len(), 3);
        let diagnostic = projection.blocks().front().unwrap();
        assert_eq!(diagnostic.title, "Diagnostic · 3 related events");
        assert_eq!(diagnostic.text, message);
        assert_eq!(projection.blocks().back().unwrap().title, "Diagnostic");
    }

    #[test]
    fn long_transcript_retains_the_newest_ten_thousand_stable_blocks() {
        let projection =
            ConversationProjection::hydrate(transcript((0..10_500).map(user).collect::<Vec<_>>()));
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(projection.omitted_blocks(), 500);
        assert_eq!(projection.blocks().front().unwrap().text, "message 500");
        assert_eq!(projection.blocks().back().unwrap().text, "message 10499");
    }

    #[test]
    fn release_fixture_retains_ten_thousand_blocks_and_at_least_ten_mib() {
        let payload = "x".repeat(1_280);
        let items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
                started_at: None,
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text, .. } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);

        let projection = ConversationProjection::hydrate(transcript(items));
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(projection.omitted_blocks(), 0);
        assert!(projection.retained_bytes() <= MAX_TRANSCRIPT_BYTES);
        assert!(projection.blocks().front().unwrap().text.starts_with("0:"));
        assert!(
            projection
                .blocks()
                .back()
                .unwrap()
                .text
                .starts_with("9999:")
        );
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_empty_conversation_baseline() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let viewport = ConversationViewport::new(30);
        let composer = ComposerState::default();
        std::hint::black_box((&projection, &viewport, &composer));
        println!("desktop_perf\tempty_blocks={}", projection.blocks().len());
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_ten_mib_interaction_baseline() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const SAMPLE_COUNT: usize = 500;
        const VISIBLE_BLOCKS: usize = 30;
        const FRAME_BUDGET_MICROS: u128 = 16_700;
        const HYDRATION_ALLOCATION_BUDGET: u64 = 40_064;
        const HYDRATION_ALLOCATED_BYTE_BUDGET: u64 = 8 * 1024 * 1024;
        const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

        let payload = format!(
            "# streamed message\n\n{}\n\n```text\n{}\n```",
            "x".repeat(1_152),
            "tool progress".repeat(8)
        );
        let items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
                started_at: None,
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text, .. } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);

        let rss_before = crate::allocation_probe::resident_bytes();
        let allocations_before = crate::allocation_probe::snapshot();
        let hydration_started = std::time::Instant::now();
        let projection = ConversationProjection::hydrate(transcript(items));
        let hydration_micros = hydration_started.elapsed().as_micros();
        let hydration_allocations =
            crate::allocation_probe::snapshot().delta_since(allocations_before);
        let rss_after = crate::allocation_probe::resident_bytes();
        let rss_growth = resident_growth(rss_before, rss_after);
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);

        let mut viewport = ConversationViewport::new(VISIBLE_BLOCKS);
        viewport.on_blocks_changed(projection.blocks().len());
        let mut scroll_samples = Vec::with_capacity(SAMPLE_COUNT);
        for sample in 0..SAMPLE_COUNT {
            let first_visible =
                (sample * 97) % (MAX_TRANSCRIPT_BLOCKS.saturating_sub(VISIBLE_BLOCKS));
            let started = std::time::Instant::now();
            viewport.user_scrolled(first_visible, projection.blocks().len());
            let visible = projection
                .blocks()
                .iter()
                .skip(viewport.first_visible())
                .take(VISIBLE_BLOCKS)
                .map(|block| bounded_markdown_preview(&block.text))
                .collect::<Vec<_>>();
            std::hint::black_box(visible);
            scroll_samples.push(started.elapsed().as_micros());
        }

        let mut composer = ComposerState::default();
        let mut input_samples = Vec::with_capacity(SAMPLE_COUNT);
        for sample in 0..SAMPLE_COUNT {
            let draft = format!("ordinary desktop input {sample} {}", "x".repeat(256));
            let started = std::time::Instant::now();
            composer.edit(draft);
            std::hint::black_box(composer.draft());
            input_samples.push(started.elapsed().as_nanos());
        }

        let scroll_p95_micros = percentile_95(&mut scroll_samples);
        let input_p95_micros = percentile_95(&mut input_samples).div_ceil(1_000);
        println!(
            "desktop_perf\tplatform={}\tfixture_bytes={fixture_bytes}\thydration_us={hydration_micros}\t\
             hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
             rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}\t\
             scroll_render_p95_us={scroll_p95_micros}\tinput_p95_us={input_p95_micros}",
            std::env::consts::OS,
            hydration_allocations.count(),
            hydration_allocations.bytes(),
            projection.retained_bytes(),
            rss_before.is_some() && rss_after.is_some(),
            rss_before.unwrap_or_default(),
            rss_after.unwrap_or_default(),
            rss_growth.unwrap_or_default()
        );
        assert!(
            scroll_p95_micros <= FRAME_BUDGET_MICROS,
            "10k transcript scroll/render preparation P95 exceeded one frame: \
             {scroll_p95_micros} us"
        );
        assert!(
            input_p95_micros <= FRAME_BUDGET_MICROS,
            "composer input P95 exceeded one frame: {input_p95_micros} us"
        );
        assert!(
            hydration_allocations.count() <= HYDRATION_ALLOCATION_BUDGET,
            "10k transcript hydration allocation count exceeded the linear budget: {}",
            hydration_allocations.count()
        );
        assert!(
            hydration_allocations.bytes() <= HYDRATION_ALLOCATED_BYTE_BUDGET,
            "10 MiB transcript hydration copied too much retained content: {} bytes",
            hydration_allocations.bytes()
        );
        if let Some(rss_growth) = rss_growth {
            assert!(
                rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
                "10 MiB transcript hydration RSS growth exceeded 64 MiB: {rss_growth} bytes"
            );
        }
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_scale_content_and_streaming_matrix() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const FRAME_BUDGET_MICROS: u128 = 16_700;
        const FINAL_PARSE_BUDGET_MICROS: u128 = 150_000;
        const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

        for block_count in [1, 100, 1_000, MAX_TRANSCRIPT_BLOCKS] {
            let items = (0..block_count)
                .map(user)
                .collect::<Vec<CodingAgentSessionTranscriptItem>>();
            let rss_before = crate::allocation_probe::resident_bytes();
            let allocations_before = crate::allocation_probe::snapshot();
            let started = std::time::Instant::now();
            let projection = ConversationProjection::hydrate(transcript(items));
            let hydration_micros = started.elapsed().as_micros();
            let hydration_allocations =
                crate::allocation_probe::snapshot().delta_since(allocations_before);
            let rss_after = crate::allocation_probe::resident_bytes();
            let rss_growth = resident_growth(rss_before, rss_after);
            println!(
                "desktop_perf\tplatform={}\tscale_blocks={block_count}\thydration_us={hydration_micros}\t\
                 hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
                 rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}",
                std::env::consts::OS,
                hydration_allocations.count(),
                hydration_allocations.bytes(),
                projection.retained_bytes(),
                rss_before.is_some() && rss_after.is_some(),
                rss_before.unwrap_or_default(),
                rss_after.unwrap_or_default(),
                rss_growth.unwrap_or_default()
            );
            assert_eq!(projection.blocks().len(), block_count);
            assert!(
                hydration_allocations.count() <= block_count as u64 * 4 + 64,
                "{block_count}-block hydration allocation count exceeded the linear budget"
            );
            assert!(
                hydration_allocations.bytes() <= block_count as u64 * 512 + 4_096,
                "{block_count}-block hydration allocated-byte count exceeded the linear budget"
            );
            if let Some(rss_growth) = rss_growth {
                assert!(
                    rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
                    "{block_count}-block hydration RSS growth exceeded 64 MiB"
                );
            }
        }

        let table_row = format!(
            "| {} |\n",
            (0..32).map(|_| "cell").collect::<Vec<_>>().join(" | ")
        );
        let content_cases = [
            (
                "markdown_256k",
                format!(
                    "# heading\n\n{}",
                    "paragraph **bold** `code`\n".repeat(10_000)
                ),
            ),
            ("reasoning_512k", "reasoning step 中文 🧠\n".repeat(24_000)),
            (
                "bash_output",
                format!("```text\n{}\n```", "build output\n".repeat(80_000)),
            ),
            ("table", table_row.repeat(1_000)),
            (
                "code_cjk_emoji",
                format!(
                    "```rust\n{}\n```\n{}",
                    "fn main() {}\n".repeat(12_000),
                    "中文🙂🚀\n".repeat(12_000)
                ),
            ),
        ];
        for (name, payload) in content_cases {
            let mut samples = Vec::with_capacity(20);
            for _ in 0..20 {
                let started = std::time::Instant::now();
                let preview = bounded_markdown_preview(&payload);
                std::hint::black_box(preview);
                samples.push(started.elapsed().as_micros());
            }
            let preview_p95_micros = percentile_95(&mut samples);
            println!(
                "desktop_perf\tcontent={name}\tinput_bytes={}\tpreview_sanitize_p95_us={preview_p95_micros}",
                payload.len()
            );
            assert!(
                preview_p95_micros <= FINAL_PARSE_BUDGET_MICROS,
                "{name} bounded preview sanitize P95 exceeded 150ms: {preview_p95_micros} us"
            );
        }

        let streaming_text = "streaming 中文 🙂 output ".repeat(128);
        for events_per_second in [10, 50, 200] {
            let mut cache = ConversationRowRenderCache::default();
            let mut samples = Vec::with_capacity(events_per_second);
            for revision in 1..=events_per_second {
                cache.begin_frame();
                let started = std::time::Instant::now();
                let row = cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            "performance-session",
                            ConversationItemKind::LiveMessage,
                            "live:assistant:performance",
                        ),
                        source_revision: revision as u64,
                        title: Cow::Borrowed("Assistant"),
                        text: &streaming_text,
                        detail: "",
                        kind: ConversationBlockKind::Assistant,
                        done: false,
                        is_error: false,
                        image_count: 0,
                        reasoning_duration_millis: None,
                        truncated: false,
                        durable: false,
                        delegation: None,
                        turn: None,
                        model: None,
                    },
                    900,
                );
                std::hint::black_box(row);
                cache.finish_incremental();
                samples.push(started.elapsed().as_micros());
            }
            let event_p95_micros = percentile_95(&mut samples);
            println!(
                "desktop_perf\tstream_events_per_s={events_per_second}\t\
                 event_p95_us={event_p95_micros}\tcache_bytes={}",
                cache.retained_bytes()
            );
            assert!(
                event_p95_micros <= FRAME_BUDGET_MICROS,
                "{events_per_second} events/s row preparation P95 exceeded one frame: \
                 {event_p95_micros} us"
            );
        }
    }

    fn percentile_95(samples: &mut [u128]) -> u128 {
        samples.sort_unstable();
        let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        samples[index]
    }

    fn resident_growth(before: Option<u64>, after: Option<u64>) -> Option<u64> {
        Some(after?.saturating_sub(before?))
    }

    #[test]
    fn long_unicode_text_is_truncated_on_a_scalar_boundary() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "界".repeat(MAX_BLOCK_TEXT_BYTES),
                started_at: None,
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert!(block.truncated);
        assert!(block.text.len() <= MAX_BLOCK_TEXT_BYTES);
        assert!(block.text.is_char_boundary(block.text.len()));
    }

    #[test]
    fn tool_duration_uses_compact_stable_units() {
        assert_eq!(compact_duration(0), "0 ms");
        assert_eq!(compact_duration(999), "999 ms");
        assert_eq!(compact_duration(1_049), "1.0 s");
        assert_eq!(compact_duration(1_050), "1.1 s");
        assert_eq!(compact_duration(59_949), "59.9 s");
        assert_eq!(compact_duration(59_950), "1m 00s");
        assert_eq!(compact_duration(60_000), "1m 00s");
        assert_eq!(compact_duration(125_600), "2m 06s");
    }

    #[test]
    fn assistant_hydration_preserves_reasoning_duration_for_disclosure() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-reasoning".into(),
                text: "answer".into(),
                thinking: "reasoning".into(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: Some(2_430),
                model_id: None,
                completed_at: None,
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert_eq!(block.reasoning_duration_millis, Some(2_430));
        assert_eq!(
            compact_duration(block.reasoning_duration_millis.unwrap()),
            "2.4 s"
        );
    }

    #[test]
    fn assistant_hydration_carries_the_message_level_model() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-model".into(),
                text: "answer".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
                model_id: Some("deepseek-v4-flash".into()),
                completed_at: None,
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert_eq!(block.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn turn_metadata_lands_on_the_final_assistant_row_of_a_completed_turn() {
        let items = vec![
            CodingAgentSessionTranscriptItem::User {
                text: "do it".into(),
                started_at: Some("2026-01-01T00:00:00Z".into()),
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-1".into(),
                text: "step one".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
                model_id: Some("deepseek-v4-flash".into()),
                completed_at: Some("2026-01-01T00:00:10Z".into()),
            },
            CodingAgentSessionTranscriptItem::Tool {
                call_id: "call-1".into(),
                name: "shell".into(),
                args: serde_json::json!({}),
                result: Some("done".into()),
                is_error: false,
                duration_millis: None,
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-2".into(),
                text: "step two".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
                model_id: Some("deepseek-v4-flash".into()),
                completed_at: Some("2026-01-01T00:00:30Z".into()),
            },
        ];
        let projection = ConversationProjection::hydrate(transcript(items));
        let blocks = projection.blocks();
        // Interior assistant rows of the turn carry no turn metadata.
        assert!(blocks[1].turn.is_none(), "interior row must stay bare");
        let turn = blocks[3]
            .turn
            .as_ref()
            .expect("the turn's final assistant row carries the summary");
        assert_eq!(turn.model, "deepseek-v4-flash");
        // Submit at 00:00:00 to completion at 00:00:30, tool call included.
        assert_eq!(turn.duration_millis, Some(30_000));
    }

    #[test]
    fn turn_metadata_omits_duration_for_unfinished_or_legacy_turns() {
        // The last assistant row never completed: the model is known but the
        // whole-turn duration is not.
        let items = vec![
            CodingAgentSessionTranscriptItem::User {
                text: "do it".into(),
                started_at: Some("2026-01-01T00:00:00Z".into()),
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-3".into(),
                text: "streaming…".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: false,
                reasoning_duration_millis: None,
                model_id: Some("deepseek-v4-flash".into()),
                completed_at: None,
            },
        ];
        let projection = ConversationProjection::hydrate(transcript(items));
        let turn = projection.blocks().back().unwrap().turn.as_ref().unwrap();
        assert_eq!(turn.model, "deepseek-v4-flash");
        assert_eq!(turn.duration_millis, None);

        // Legacy session without submit timestamps: no duration either.
        let items = vec![
            CodingAgentSessionTranscriptItem::User {
                text: "legacy".into(),
                started_at: None,
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-4".into(),
                text: "old answer".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
                model_id: Some("claude-sonnet-4-5".into()),
                completed_at: None,
            },
        ];
        let projection = ConversationProjection::hydrate(transcript(items));
        let turn = projection.blocks().back().unwrap().turn.as_ref().unwrap();
        assert_eq!(turn.model, "claude-sonnet-4-5");
        assert_eq!(turn.duration_millis, None);
    }
}
