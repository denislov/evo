//! Bounded conversation presentation model.
//!
//! The stable `conversation::` surface is re-exported here while each pure
//! responsibility has a single internal owner.

mod composer;
pub(crate) mod composer_pane;
pub(crate) mod controller;
mod copy;
pub(crate) mod header;
pub(crate) mod layout;
pub(crate) mod markdown;
pub(crate) mod model;
pub(crate) mod pane;
mod render_cache;
mod viewport;

pub use composer::{
    ComposerAdmission, ComposerState, ComposerSubmissionKind, SubmittedPromptPreview,
};
pub use copy::{MAX_COPY_BYTES, conversation_copy_text};
pub use layout::{
    ConversationRowLayoutInput, ConversationRowLayoutState, ConversationRowMeasurement,
    TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, conversation_width_bucket,
};
pub use model::{
    ConversationBlockKind, ConversationItemKey, ConversationItemKind, ConversationProjection,
    compact_duration,
};
pub use render_cache::{
    ConversationRowRenderCache, ConversationRowRenderData, ConversationRowRenderSource,
    StreamingTextPhase, conversation_block_height,
};
pub use viewport::ConversationViewport;
