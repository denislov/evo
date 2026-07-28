//! Bounded conversation presentation model.
//!
//! The stable `conversation::` surface is re-exported here while each pure
//! responsibility has a single internal owner.

mod composer;
mod copy;
mod layout;
mod markdown;
mod model;
mod render_cache;
mod viewport;

#[allow(unused_imports)]
pub use composer::{
    ComposerAdmission, ComposerState, ComposerSubmissionKind, ComposerSubmitError,
    MAX_COMPOSER_BYTES, SubmittedPromptPreview,
};
pub use copy::{MAX_COPY_BYTES, conversation_copy_text};
#[allow(unused_imports)]
pub use layout::{
    CONVERSATION_WIDTH_BUCKET_PX, ConversationRowHeightSource, ConversationRowLayoutInput,
    ConversationRowLayoutResolution, ConversationRowLayoutSingleResolution,
    ConversationRowLayoutState, ConversationRowMeasurement, STREAMING_ROW_HEIGHT_INTERVAL,
    TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, conversation_width_bucket,
};
#[allow(unused_imports)]
pub use markdown::{
    MAX_CODE_BLOCK_PREVIEW_BYTES, MAX_MARKDOWN_LINE_BYTES, MAX_MARKDOWN_LINES,
    MAX_MARKDOWN_MARKERS_PER_LINE, MAX_MARKDOWN_NESTING, MAX_MARKDOWN_PREVIEW_BYTES,
    MAX_MARKDOWN_TABLE_CELLS, MAX_MARKDOWN_TABLE_ROWS, MarkdownPreview, bounded_markdown_preview,
};
#[allow(unused_imports)]
pub use model::{
    ConversationBlock, ConversationBlockKind, ConversationItemKey, ConversationItemKind,
    ConversationProjection, MAX_BLOCK_TEXT_BYTES, MAX_THINKING_TEXT_BYTES, MAX_TOOL_ARGUMENT_BYTES,
    MAX_TRANSCRIPT_BLOCKS, MAX_TRANSCRIPT_BYTES, compact_duration,
};
#[allow(unused_imports)]
pub use render_cache::{
    ConversationRowRenderCache, ConversationRowRenderData, ConversationRowRenderSource,
    MAX_ROW_RENDER_CACHE_BYTES, MAX_ROW_RENDER_CACHE_ENTRIES, MAX_SETTLING_MARKDOWN_BYTES,
    STREAMING_MARKDOWN_SETTLE_DELAY, StreamingTextPhase, conversation_block_height,
};
#[allow(unused_imports)]
pub use viewport::{
    ConversationViewport, FOLLOW_LATEST_PAUSE_THRESHOLD_PX, FOLLOW_LATEST_RESUME_THRESHOLD_PX,
};
