//! Bounded conversation, viewport, selection, and composer state.
//!
//! These reducers remain independent of GPUI. The renderer may virtualize the
//! resulting blocks without owning product transcript truth.

use std::collections::VecDeque;

use coding_agent::api::view::{CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot};

pub const MAX_TRANSCRIPT_BLOCKS: usize = 10_000;
pub const MAX_TRANSCRIPT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BLOCK_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_THINKING_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
pub const MAX_COPY_BYTES: usize = 1024 * 1024;
pub const MAX_COMPOSER_BYTES: usize = 1024 * 1024;
pub const MAX_MARKDOWN_PREVIEW_BYTES: usize = 256 * 1024;
pub const MAX_MARKDOWN_LINE_BYTES: usize = 16 * 1024;
pub const MAX_MARKDOWN_LINES: usize = 4_096;
pub const MAX_MARKDOWN_NESTING: usize = 24;
pub const MAX_MARKDOWN_MARKERS_PER_LINE: usize = 128;
pub const MAX_MARKDOWN_TABLE_ROWS: usize = 256;
pub const MAX_MARKDOWN_TABLE_CELLS: usize = 64;
pub const MAX_CODE_BLOCK_PREVIEW_BYTES: usize = 128 * 1024;

const TRUNCATED_LINE_NOTICE: &str = "\n\n> … line truncated by desktop preview bounds …\n";
const TRUNCATED_CODE_NOTICE: &str = "\n… code block truncated by desktop preview bounds …\n";
const TRUNCATED_TABLE_NOTICE: &str = "\n\n> … table rows omitted by desktop preview bounds …\n";
const TRUNCATED_DOCUMENT_NOTICE: &str = "\n\n> … document truncated by desktop preview bounds …\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownPreview {
    pub text: String,
    pub truncated: bool,
    pub media_neutralized: bool,
}

/// Produce bounded Markdown for the native renderer without mutating copy text.
///
/// Model-authored HTML and Markdown images are neutralized because rendering a
/// conversation must not initiate ambient media loading. Explicit product
/// image attachments remain represented by the transcript block's image count.
pub fn bounded_markdown_preview(raw: &str) -> MarkdownPreview {
    let mut preview = MarkdownPreviewBuilder::new();
    let mut fence = None;
    let mut code_bytes = 0;
    let mut code_notice_emitted = false;
    let mut consecutive_table_rows = 0;
    let mut table_notice_emitted = false;
    let mut lines = raw.split_inclusive('\n');

    for line_index in 0..MAX_MARKDOWN_LINES {
        let Some(raw_line) = lines.next() else {
            break;
        };
        let has_newline = raw_line.ends_with('\n');
        let line_without_newline = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let (line, line_truncated) = truncate_str(line_without_newline, MAX_MARKDOWN_LINE_BYTES);
        preview.truncated |= line_truncated;

        if let Some(delimiter) = fence_delimiter(line) {
            let closes_fence = fence.is_some_and(|open| open == delimiter);
            if fence.is_none() || closes_fence {
                preview.push(line);
                if has_newline {
                    preview.push("\n");
                }
                if closes_fence {
                    fence = None;
                    code_bytes = 0;
                    code_notice_emitted = false;
                } else {
                    fence = Some(delimiter);
                }
                continue;
            }
        }

        if fence.is_some() {
            let remaining = MAX_CODE_BLOCK_PREVIEW_BYTES.saturating_sub(code_bytes);
            let (bounded_code, code_truncated) = truncate_str(line, remaining);
            preview.push(bounded_code);
            code_bytes = code_bytes.saturating_add(bounded_code.len());
            if has_newline && !code_truncated {
                preview.push("\n");
                code_bytes = code_bytes.saturating_add(1);
            }
            if code_truncated || remaining == 0 {
                preview.truncated = true;
                if !code_notice_emitted {
                    preview.push(TRUNCATED_CODE_NOTICE);
                    code_notice_emitted = true;
                }
            }
            continue;
        }

        let (line, nesting_truncated) = cap_markdown_nesting(line);
        preview.truncated |= nesting_truncated;
        let table_cells = line.bytes().filter(|byte| *byte == b'|').count();
        let is_table_row = table_cells >= 2;
        if is_table_row {
            consecutive_table_rows += 1;
            if consecutive_table_rows > MAX_MARKDOWN_TABLE_ROWS {
                preview.truncated = true;
                if !table_notice_emitted {
                    preview.push(TRUNCATED_TABLE_NOTICE);
                    table_notice_emitted = true;
                }
                continue;
            }
        } else {
            consecutive_table_rows = 0;
            table_notice_emitted = false;
        }

        push_safe_markdown_line(&mut preview, &line);
        if has_newline {
            preview.push("\n");
        }
        if line_truncated {
            preview.push(TRUNCATED_LINE_NOTICE);
        }
        if preview.full() {
            break;
        }

        if line_index + 1 == MAX_MARKDOWN_LINES && lines.next().is_some() {
            preview.truncated = true;
            preview.push(TRUNCATED_DOCUMENT_NOTICE);
        }
    }

    if raw.len() > MAX_MARKDOWN_PREVIEW_BYTES || !preview.consumed_all_capacity() {
        preview.truncated = true;
    }
    preview.finish()
}

struct MarkdownPreviewBuilder {
    text: String,
    truncated: bool,
    media_neutralized: bool,
    capacity_exhausted: bool,
}

impl MarkdownPreviewBuilder {
    fn new() -> Self {
        Self {
            text: String::with_capacity(MAX_MARKDOWN_PREVIEW_BYTES.min(16 * 1024)),
            truncated: false,
            media_neutralized: false,
            capacity_exhausted: false,
        }
    }

    fn push(&mut self, value: &str) {
        let remaining = MAX_MARKDOWN_PREVIEW_BYTES.saturating_sub(self.text.len());
        let (value, truncated) = truncate_str(value, remaining);
        self.text.push_str(value);
        self.truncated |= truncated;
        self.capacity_exhausted |= truncated;
    }

    fn full(&self) -> bool {
        self.text.len() == MAX_MARKDOWN_PREVIEW_BYTES
    }

    fn consumed_all_capacity(&self) -> bool {
        !self.capacity_exhausted
    }

    fn finish(self) -> MarkdownPreview {
        MarkdownPreview {
            text: self.text,
            truncated: self.truncated,
            media_neutralized: self.media_neutralized,
        }
    }
}

fn fence_delimiter(line: &str) -> Option<char> {
    let line = line.trim_start();
    let delimiter = line.chars().next()?;
    if !matches!(delimiter, '`' | '~') {
        return None;
    }
    (line
        .chars()
        .take_while(|character| *character == delimiter)
        .count()
        >= 3)
        .then_some(delimiter)
}

fn cap_markdown_nesting(line: &str) -> (String, bool) {
    let leading_spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    let retained_spaces = leading_spaces.min(MAX_MARKDOWN_NESTING * 4);
    let mut rest = &line[leading_spaces..];
    let mut blockquotes = 0;
    let mut prefix_bytes = 0;
    while let Some(after) = rest.strip_prefix('>') {
        blockquotes += 1;
        prefix_bytes += 1;
        rest = after;
        if let Some(after_space) = rest.strip_prefix(' ') {
            prefix_bytes += 1;
            rest = after_space;
        }
    }
    let retained_blockquotes = blockquotes.min(MAX_MARKDOWN_NESTING);
    let truncated = leading_spaces != retained_spaces || blockquotes != retained_blockquotes;
    if !truncated {
        return (line.to_owned(), false);
    }

    let mut bounded = String::with_capacity(line.len());
    bounded.push_str(&" ".repeat(retained_spaces));
    for _ in 0..retained_blockquotes {
        bounded.push_str("> ");
    }
    bounded.push_str(&line[leading_spaces + prefix_bytes..]);
    (bounded, true)
}

fn push_safe_markdown_line(preview: &mut MarkdownPreviewBuilder, line: &str) {
    let mut characters = line.chars().peekable();
    let mut marker_count = 0;
    let mut table_cells = 0;
    while let Some(character) = characters.next() {
        if character == '!' && characters.peek() == Some(&'[') {
            preview.push("\\!");
            preview.media_neutralized = true;
            continue;
        }
        if character == '<' {
            preview.push("&lt;");
            preview.media_neutralized = true;
            continue;
        }
        if character == '|' {
            table_cells += 1;
            if table_cells > MAX_MARKDOWN_TABLE_CELLS {
                preview.push("\\|");
                preview.truncated = true;
                continue;
            }
        }
        if matches!(character, '*' | '_' | '~') {
            marker_count += 1;
            if marker_count > MAX_MARKDOWN_MARKERS_PER_LINE {
                preview.push("\\");
                preview.truncated = true;
            }
        }
        let mut encoded = [0; 4];
        preview.push(character.encode_utf8(&mut encoded));
    }
}

fn truncate_str(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (&text[..boundary], true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBlock {
    pub id: String,
    pub kind: ConversationBlockKind,
    pub title: String,
    pub text: String,
    pub detail: String,
    pub done: bool,
    pub is_error: bool,
    pub image_count: usize,
    pub truncated: bool,
}

impl ConversationBlock {
    pub fn copy_text(&self) -> String {
        let mut text = self.text.clone();
        if !self.detail.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&self.detail);
        }
        truncate_bytes(text, MAX_COPY_BYTES).0
    }

    fn retained_bytes(&self) -> usize {
        self.id.len() + self.title.len() + self.text.len() + self.detail.len()
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
        let mut projection = Self {
            session_id: snapshot.session_id,
            active_leaf_id: snapshot.active_leaf_id,
            blocks: VecDeque::new(),
            omitted_blocks: 0,
            retained_bytes: 0,
        };
        for (index, item) in snapshot.items.into_iter().enumerate() {
            projection.push_bounded(block_from_product(index, item));
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationViewport {
    selected_block_id: Option<String>,
    first_visible: usize,
    visible_count: usize,
    follow_latest: bool,
}

impl ConversationViewport {
    pub fn new(visible_count: usize) -> Self {
        Self {
            selected_block_id: None,
            first_visible: 0,
            visible_count: visible_count.max(1),
            follow_latest: true,
        }
    }

    pub fn selected_block_id(&self) -> Option<&str> {
        self.selected_block_id.as_deref()
    }

    #[cfg(test)]
    pub const fn first_visible(&self) -> usize {
        self.first_visible
    }

    pub const fn follow_latest(&self) -> bool {
        self.follow_latest
    }

    pub fn pause_follow_latest(&mut self) {
        self.follow_latest = false;
    }

    pub fn resume_latest(&mut self, block_count: usize) {
        self.follow_latest = true;
        self.on_blocks_changed(block_count);
    }

    pub fn select(&mut self, block_id: impl Into<String>, projection: &ConversationProjection) {
        let block_id = block_id.into();
        if projection.block(&block_id).is_some() {
            self.selected_block_id = Some(block_id);
        }
    }

    /// Select a currently visible live overlay that is not committed yet.
    ///
    /// The overlay must use the same typed message/tool identity as its future
    /// durable block so hydration can preserve the selection.
    pub fn select_live(&mut self, block_id: impl Into<String>) {
        self.selected_block_id = Some(block_id.into());
    }

    pub fn reconcile_hydration(&mut self, projection: &ConversationProjection) {
        if self
            .selected_block_id
            .as_deref()
            .is_some_and(|id| projection.block(id).is_none())
        {
            self.selected_block_id = None;
        }
        self.on_blocks_changed(projection.blocks.len());
    }

    pub fn reconcile_live_selection(&mut self, live_id: &str, durable_id: &str) {
        if self.selected_block_id.as_deref() == Some(live_id) {
            self.selected_block_id = Some(durable_id.to_owned());
        }
    }

    #[cfg(test)]
    pub fn user_scrolled(&mut self, first_visible: usize, block_count: usize) {
        let max_first = block_count.saturating_sub(self.visible_count);
        self.first_visible = first_visible.min(max_first);
        self.follow_latest = self.first_visible.saturating_add(self.visible_count) >= block_count;
    }

    pub fn on_blocks_changed(&mut self, block_count: usize) {
        let max_first = block_count.saturating_sub(self.visible_count);
        if self.follow_latest {
            self.first_visible = max_first;
        } else {
            self.first_visible = self.first_visible.min(max_first);
        }
    }

    #[cfg(test)]
    pub fn home(&mut self, projection: &ConversationProjection) {
        self.follow_latest = false;
        self.first_visible = 0;
        self.selected_block_id = projection.blocks.front().map(|block| block.id.clone());
    }

    #[cfg(test)]
    pub fn end(&mut self, projection: &ConversationProjection) {
        self.follow_latest = true;
        self.on_blocks_changed(projection.blocks.len());
        self.selected_block_id = projection.blocks.back().map(|block| block.id.clone());
    }

    pub fn copy_selected(&self, projection: &ConversationProjection) -> Option<String> {
        projection
            .block(self.selected_block_id.as_deref()?)
            .map(ConversationBlock::copy_text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAdmission {
    Idle,
    Pending {
        command_id: u64,
        kind: ComposerSubmissionKind,
        payload: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmissionKind {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedPromptPreview {
    pub command_id: u64,
    pub payload: String,
}

impl SubmittedPromptPreview {
    pub fn block_id(&self) -> String {
        format!("submitted-user:{}", self.command_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposerSubmitError {
    #[error("composer draft is empty")]
    Empty,
    #[error("composer draft exceeds {MAX_COMPOSER_BYTES} bytes")]
    TooLarge,
    #[error("composer submission is already awaiting admission")]
    AdmissionPending,
    #[error("composer completion does not match pending command")]
    StaleCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerState {
    draft: String,
    admission: ComposerAdmission,
    submitted: Option<SubmittedPromptPreview>,
    rejection: Option<String>,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            admission: ComposerAdmission::Idle,
            submitted: None,
            rejection: None,
        }
    }
}

impl ComposerState {
    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn admission(&self) -> &ComposerAdmission {
        &self.admission
    }

    pub fn rejection(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    pub fn submitted(&self) -> Option<&SubmittedPromptPreview> {
        self.submitted.as_ref()
    }

    pub fn edit(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
        self.rejection = None;
    }

    pub fn begin_submit(
        &mut self,
        command_id: u64,
        kind: ComposerSubmissionKind,
    ) -> Result<&str, ComposerSubmitError> {
        if matches!(self.admission, ComposerAdmission::Pending { .. }) {
            return Err(ComposerSubmitError::AdmissionPending);
        }
        if self.draft.trim().is_empty() {
            return Err(ComposerSubmitError::Empty);
        }
        if self.draft.len() > MAX_COMPOSER_BYTES {
            return Err(ComposerSubmitError::TooLarge);
        }
        self.admission = ComposerAdmission::Pending {
            command_id,
            kind,
            payload: self.draft.clone(),
        };
        let ComposerAdmission::Pending { payload, .. } = &self.admission else {
            unreachable!("composer admission was just installed");
        };
        Ok(payload)
    }

    pub fn accepted(&mut self, command_id: u64) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            kind,
            payload,
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        if *kind == ComposerSubmissionKind::Prompt {
            self.submitted = Some(SubmittedPromptPreview {
                command_id,
                payload: payload.clone(),
            });
        }
        if self.draft == *payload {
            self.draft.clear();
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = None;
        Ok(())
    }

    /// Reconcile an accepted client-local prompt with completed durable truth.
    ///
    /// Returns the live and durable block identities when the prompt was
    /// retained. If completed hydration does not contain it, the exact payload
    /// is restored to the draft instead of being silently lost.
    pub fn reconcile_completed_submission(
        &mut self,
        projection: &ConversationProjection,
    ) -> Option<(String, String)> {
        let submitted = self.submitted.take()?;
        if let Some(block) = projection.blocks().iter().rev().find(|block| {
            block.kind == ConversationBlockKind::User && block.text == submitted.payload
        }) {
            self.rejection = None;
            return Some((submitted.block_id(), block.id.clone()));
        }
        if self.draft.is_empty() {
            self.draft = submitted.payload;
        }
        self.rejection =
            Some("Accepted prompt was not retained; the exact draft was restored.".into());
        None
    }

    pub fn rejected(
        &mut self,
        command_id: u64,
        message: impl Into<String>,
    ) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            ..
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = Some(truncate_bytes(message.into(), MAX_BLOCK_TEXT_BYTES).0);
        Ok(())
    }
}

fn block_from_product(index: usize, item: CodingAgentSessionTranscriptItem) -> ConversationBlock {
    let (kind, source_id, title, text, detail, done, is_error, image_count, truncated) = match item
    {
        CodingAgentSessionTranscriptItem::User { text } => {
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
            )
        }
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
        } => {
            let (text, text_truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
            let (thinking, thinking_truncated) = truncate_bytes(thinking, MAX_THINKING_TEXT_BYTES);
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
            )
        }
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
        } => {
            let arguments = serde_json::to_string_pretty(&args)
                .unwrap_or_else(|_| "<invalid tool arguments>".into());
            let (arguments, args_truncated) = truncate_bytes(arguments, MAX_TOOL_ARGUMENT_BYTES);
            let (result, result_truncated) =
                truncate_bytes(result.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::Tool,
                call_id,
                format!("Tool · {name}"),
                result,
                arguments,
                true,
                is_error,
                0,
                args_truncated || result_truncated,
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
                truncate_bytes(summary.unwrap_or(status), MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::Delegation,
                tool_call_id,
                format!("Delegation · {target_kind:?} · {target_id}"),
                task,
                summary,
                true,
                false,
                0,
                task_truncated || summary_truncated,
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
            )
        }
    };
    let id = if source_id.is_empty() {
        format!("{index:08}:{}", kind.key())
    } else {
        format!("{}:{source_id}", kind.key())
    };
    ConversationBlock {
        id,
        kind,
        title,
        text,
        detail,
        done,
        is_error,
        image_count,
        truncated,
    }
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
    )
}

fn truncate_bytes(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    (text, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(items: Vec<CodingAgentSessionTranscriptItem>) -> CodingAgentTranscriptSnapshot {
        CodingAgentTranscriptSnapshot {
            session_id: "session-1".into(),
            active_leaf_id: Some("leaf-1".into()),
            items,
        }
    }

    fn user(index: usize) -> CodingAgentSessionTranscriptItem {
        CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}"),
        }
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
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text } => text.len(),
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
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let viewport = ConversationViewport::new(30);
        let composer = ComposerState::default();
        std::hint::black_box((&projection, &viewport, &composer));
        println!("desktop_perf\tempty_blocks={}", projection.blocks().len());
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_ten_mib_interaction_baseline() {
        const SAMPLE_COUNT: usize = 500;
        const VISIBLE_BLOCKS: usize = 30;
        const FRAME_BUDGET_MICROS: u128 = 16_700;

        let payload = format!(
            "# streamed message\n\n{}\n\n```text\n{}\n```",
            "x".repeat(1_152),
            "tool progress".repeat(8)
        );
        let items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);

        let hydration_started = std::time::Instant::now();
        let projection = ConversationProjection::hydrate(transcript(items));
        let hydration_micros = hydration_started.elapsed().as_micros();
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
            "desktop_perf\tfixture_bytes={fixture_bytes}\thydration_us={hydration_micros}\t\
             scroll_render_p95_us={scroll_p95_micros}\tinput_p95_us={input_p95_micros}"
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
    }

    fn percentile_95(samples: &mut [u128]) -> u128 {
        samples.sort_unstable();
        let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        samples[index]
    }

    #[test]
    fn long_unicode_text_is_truncated_on_a_scalar_boundary() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "界".repeat(MAX_BLOCK_TEXT_BYTES),
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert!(block.truncated);
        assert!(block.text.len() <= MAX_BLOCK_TEXT_BYTES);
        assert!(block.text.is_char_boundary(block.text.len()));
    }

    #[test]
    fn markdown_preview_neutralizes_ambient_media_and_bounds_nesting() {
        let raw = format!(
            "{}<img src=\"https://example.invalid/a.png\"> ![alt](https://example.invalid/b.png)",
            "> ".repeat(MAX_MARKDOWN_NESTING + 100)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.media_neutralized);
        assert!(!preview.text.contains("<img"));
        assert!(preview.text.contains("\\![alt]"));
        assert_eq!(
            preview
                .text
                .chars()
                .take_while(|character| matches!(character, '>' | ' '))
                .filter(|character| *character == '>')
                .count(),
            MAX_MARKDOWN_NESTING
        );
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
    }

    #[test]
    fn markdown_preview_bounds_code_lines_tables_and_marker_pressure() {
        let code_line = format!("{}\n", "界".repeat(2_048));
        let code = code_line.repeat(100);
        let wide_table = format!(
            "{}\n",
            std::iter::repeat_n("cell", MAX_MARKDOWN_TABLE_CELLS + 20)
                .collect::<Vec<_>>()
                .join("|")
        );
        let tables = wide_table.repeat(MAX_MARKDOWN_TABLE_ROWS + 20);
        let raw = format!(
            "```\n{code}\n```\n{}\n{tables}",
            "*".repeat(MAX_MARKDOWN_MARKERS_PER_LINE + 20)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.text.contains("code block truncated"));
        assert!(preview.text.contains("table rows omitted"));
        assert!(preview.text.contains("\\*"));
        assert!(preview.text.contains("\\|"));
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
        assert!(preview.text.is_char_boundary(preview.text.len()));
    }

    #[test]
    fn fenced_code_does_not_treat_literal_html_as_media() {
        let preview = bounded_markdown_preview("```html\n<img src=\"literal\">\n```\n");
        assert!(!preview.truncated);
        assert!(!preview.media_neutralized);
        assert!(preview.text.contains("<img src=\"literal\">"));
    }

    #[test]
    fn malformed_long_markdown_remains_bounded_and_unicode_valid() {
        let raw = format!(
            "{}{}",
            "[*".repeat(MAX_MARKDOWN_LINE_BYTES),
            "\n界".repeat(MAX_MARKDOWN_LINES + 100)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
        assert!(preview.text.is_char_boundary(preview.text.len()));
    }

    #[test]
    fn selection_survives_matching_hydration_and_copy_is_bounded() {
        let projection = ConversationProjection::hydrate(transcript(vec![user(0), user(1)]));
        let selected = projection.blocks().front().unwrap().id.clone();
        let mut viewport = ConversationViewport::new(1);
        viewport.select(selected.clone(), &projection);
        viewport.reconcile_hydration(&projection);
        assert_eq!(viewport.selected_block_id(), Some(selected.as_str()));
        assert_eq!(
            viewport.copy_selected(&projection).as_deref(),
            Some("message 0")
        );
    }

    #[test]
    fn tool_copy_includes_arguments_and_result_without_exceeding_the_copy_cap() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Tool {
                call_id: "call-1".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "x".repeat(MAX_TOOL_ARGUMENT_BYTES)}),
                result: Some("界".repeat(MAX_BLOCK_TEXT_BYTES)),
                is_error: false,
            },
        ]));
        let block = projection.blocks().front().unwrap();
        let copied = block.copy_text();
        assert!(copied.len() <= MAX_COPY_BYTES);
        assert!(copied.is_char_boundary(copied.len()));
        assert!(block.truncated);
    }

    #[test]
    fn live_assistant_selection_reconciles_to_the_durable_identity() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-7".into(),
                text: "done".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
            },
        ]));
        let mut viewport = ConversationViewport::new(1);
        viewport.select_live("assistant:message-7");
        viewport.reconcile_hydration(&projection);
        assert_eq!(viewport.selected_block_id(), Some("assistant:message-7"));
    }

    #[test]
    fn scrolled_reader_is_not_forced_to_latest() {
        let projection = ConversationProjection::hydrate(transcript((0..20).map(user).collect()));
        let mut viewport = ConversationViewport::new(5);
        viewport.on_blocks_changed(projection.blocks().len());
        assert_eq!(viewport.first_visible(), 15);
        assert!(viewport.follow_latest());

        viewport.user_scrolled(4, projection.blocks().len());
        assert!(!viewport.follow_latest());
        viewport.on_blocks_changed(25);
        assert_eq!(viewport.first_visible(), 4);

        viewport.end(&projection);
        assert!(viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 15);
        viewport.home(&projection);
        assert_eq!(viewport.first_visible(), 0);
    }

    #[test]
    fn explicit_pause_and_resume_control_follow_latest() {
        let mut viewport = ConversationViewport::new(5);
        viewport.on_blocks_changed(20);
        assert_eq!(viewport.first_visible(), 15);

        viewport.pause_follow_latest();
        viewport.on_blocks_changed(25);
        assert!(!viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 15);

        viewport.resume_latest(25);
        assert!(viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 20);
    }

    #[test]
    fn composer_submits_exactly_once_and_retains_rejected_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        assert_eq!(
            composer
                .begin_submit(7, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        assert_eq!(
            composer.begin_submit(8, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::AdmissionPending)
        );
        composer.rejected(7, "queue full").unwrap();
        assert_eq!(composer.draft(), "  exact payload\n");
        assert_eq!(composer.rejection(), Some("queue full"));

        assert_eq!(
            composer
                .begin_submit(9, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        composer.accepted(9).unwrap();
        assert_eq!(composer.draft(), "");
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
        assert_eq!(
            composer.submitted(),
            Some(&SubmittedPromptPreview {
                command_id: 9,
                payload: "  exact payload\n".into(),
            })
        );
    }

    #[test]
    fn submitted_prompt_reconciles_selection_to_durable_user_block() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "exact payload".into(),
            },
        ]));
        let mut composer = ComposerState::default();
        composer.edit("exact payload");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();
        let live_id = composer.submitted().unwrap().block_id();
        let mut viewport = ConversationViewport::new(5);
        viewport.select_live(live_id.clone());

        let (reconciled_live, durable_id) = composer
            .reconcile_completed_submission(&projection)
            .unwrap();
        assert_eq!(reconciled_live, live_id);
        viewport.reconcile_live_selection(&reconciled_live, &durable_id);
        viewport.reconcile_hydration(&projection);
        assert_eq!(viewport.selected_block_id(), Some(durable_id.as_str()));
        assert!(composer.submitted().is_none());
        assert!(composer.rejection().is_none());
    }

    #[test]
    fn completed_hydration_without_submitted_prompt_restores_exact_draft() {
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();

        assert!(
            composer
                .reconcile_completed_submission(&projection)
                .is_none()
        );
        assert_eq!(composer.draft(), "  exact payload\n");
        assert!(composer.rejection().unwrap().contains("not retained"));
    }

    #[test]
    fn accepted_steer_clears_exact_draft_without_creating_user_overlay() {
        let mut composer = ComposerState::default();
        composer.edit("steer exactly");
        assert_eq!(
            composer
                .begin_submit(8, ComposerSubmissionKind::Steer)
                .unwrap(),
            "steer exactly"
        );
        composer.accepted(8).unwrap();
        assert!(composer.draft().is_empty());
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn rejected_follow_up_retains_exact_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  follow up exactly\n");
        composer
            .begin_submit(9, ComposerSubmissionKind::FollowUp)
            .unwrap();
        composer.rejected(9, "operation completed").unwrap();

        assert_eq!(composer.draft(), "  follow up exactly\n");
        assert_eq!(composer.rejection(), Some("operation completed"));
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn composer_rejects_empty_oversized_and_stale_completion() {
        let mut composer = ComposerState::default();
        assert_eq!(
            composer.begin_submit(1, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::Empty)
        );
        composer.edit("x".repeat(MAX_COMPOSER_BYTES + 1));
        assert_eq!(
            composer.begin_submit(2, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::TooLarge)
        );
        composer.edit("valid");
        composer
            .begin_submit(3, ComposerSubmissionKind::Prompt)
            .unwrap();
        assert_eq!(
            composer.accepted(4),
            Err(ComposerSubmitError::StaleCompletion)
        );
        assert_eq!(composer.draft(), "valid");
    }
}
