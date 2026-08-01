use super::*;
use std::{cell::RefCell, collections::HashSet, fs, rc::Rc};

use crate::application::catalog::ProjectCatalogState;
use crate::runtime::{DesktopPromptTarget, DesktopRuntimeOwnerTarget};
use crate::ui::conversation::header::header_runtime_status_slot_width;
use crate::ui::conversation::pane::CONVERSATION_RAIL_WIDTH;

use desktop::projection::{DesktopProjectionLifecycle, ProjectionEvent};
use desktop::runtime::MAX_PROMPT_ATTACHMENTS;
use desktop::ui::conversation::{ComposerAdmission, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT};
use desktop::ui::inspector::review::DesktopFileReviewDocument;
use gpui::{Role, div, px, size};

use coding_agent::api::authorization::{
    ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
    ToolAuthorizationScope,
};
use coding_agent::api::client::{
    CodingAgentContextSnapshot, CodingAgentFileChangeSnapshot, CodingAgentRecoveryPending,
    CodingAgentSnapshot, CodingAgentSnapshotCursor, UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentProfileChoice,
    CodingAgentResourceCommandKind, CodingAgentResourceSummary, CodingAgentSettingsSummary,
    CodingAgentThinkingCapability, CodingAgentThinkingLevel,
};
use coding_agent::api::review::CodingAgentFileReview;
use coding_agent::api::view::{
    CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
    CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
};
use gpui::TestAppContext;
use gpui_component::{Theme, ThemeMode, scroll::ScrollbarHandle, text::TextViewState};

use desktop::ui::shell::{
    COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, CONVERSATION_ROW_VERTICAL_PADDING_PX,
};

include!("fixtures.rs");

mod commands;
mod focus;
mod overlays;
mod performance;
mod responsive;
mod runtime_updates;
mod workspace;
