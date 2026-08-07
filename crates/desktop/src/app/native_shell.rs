use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceCommand, CodingAgentWorkspaceSelection,
};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::platform::preferences::{PreferenceWriteResult, PreferenceWriter};
use desktop::preferences::{DesktopPreferences, DesktopThinkingLevel};
use desktop::projection::{DesktopProjection, DesktopRecoveryStatus};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeSelectionKind, validate_prompt_attachments,
};
use desktop::ui::conversation::{
    ComposerSubmissionKind, MAX_COPY_BYTES, conversation_copy_text, conversation_width_bucket,
};
use desktop::ui::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH,
    CONVERSATION_CONTENT_MAX_WIDTH, FocusTarget, MIN_CONVERSATION_WIDTH, PanelVisibility,
    SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_WIDTH, SemanticTheme,
    ShellLayout, UI_FONT_FAMILY, truncate_label,
};
use gpui::{
    ClipboardItem, Context, KeyDownEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, ScrollStrategy, Window, WindowBounds, prelude::*,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CopySelectedConversation, DesktopPaletteCommand, EscapeHierarchy,
    FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput, NewSession,
    OpenCommandPalette, OpenFileSurface, PaletteConfirm, PaletteNext, PalettePrevious,
    SelectNextConversation, SelectPreviousConversation, ToggleInspectorPanel,
    ToggleSelectedConversationDetails, TrapOverlayFocus,
};
use crate::application::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    commands::{CommandTracker, DesktopCommandIntent},
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        PlatformOutcome, PlatformResult,
    },
    reducer::{
        DesktopController, DesktopEvent, PlatformUpdatePort, PreferencePanel, PreferencesIntent,
        Transition,
    },
    state::DesktopState,
    workspace::{SessionId, WorkspaceKey, WorkspaceStore},
    workspace_state::{DesktopFileReviewState, MAX_SESSION_WORKSPACES, RuntimeWorkspaceDefaults},
};
#[cfg(feature = "desktop-devtools")]
pub(super) use crate::ui::components::brand::{EvoBrandFixture, EvoBrandMode};
use crate::ui::shell::{
    ShellConnection, ShellUiState, ShellViews, presentation::recovery_action_label,
};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

mod intent;
mod intents;
mod presentation;
mod session;

pub(crate) use self::presentation::{
    NativeDesktopState, NativeShell, SessionWorkspace, build_session_workspace,
    conversation_block_visual, conversation_focus_accent, delegation_status_color,
    semantic_status_color, session_workspace_with_thinking,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum InspectorSection {
    #[default]
    Changes,
    Task,
    Usage,
    Runtime,
}

/// Typed states installed by the deterministic native visual replay.
///
/// These are presentation fixtures, not a second catalog implementation: each
/// variant drives the same [`ProjectCatalogController`] transitions used by
/// runtime updates, so reviewed images exercise production state rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "desktop-devtools")]
pub(super) enum NativeVisualCatalogFixture {
    NotLoaded,
    Loading,
    Ready,
    Error,
    Empty,
}

/// Responsive drawer selected by a deterministic native visual replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "desktop-devtools")]
pub(super) enum NativeVisualDrawerFixture {
    Sessions,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizablePanel {
    Sessions,
    Context,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelResizeState {
    pub(crate) panel: ResizablePanel,
    pub(crate) pointer_origin_x: f32,
    pub(crate) width_origin: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FocusInputModality {
    Keyboard,
    #[default]
    Pointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopModalKind {
    Authorization,
    CommandPalette,
    FullMessage,
    Search,
    ConfirmDeleteSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionDeleteConfirm {
    pub(crate) session_id: String,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationFullMessageView {
    pub(crate) block_id: String,
    pub(crate) title: Arc<str>,
    pub(crate) text: Arc<str>,
    pub(crate) source_truncated: bool,
}

#[derive(Default)]

struct RuntimePoll {
    transition: Transition,
    running: bool,
}

pub(super) struct NativeShellInit {
    pub(super) runtime: DesktopRuntimeBridge,
    pub(super) workspace: NativeShellWorkspaceInit,
    pub(super) projectless_workspace_selection: CodingAgentWorkspaceSelection,
    pub(super) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(super) preferences: DesktopPreferences,
    pub(super) preference_writer: Option<PreferenceWriter>,
    pub(super) preference_notice: Option<String>,
}

pub(super) enum NativeShellWorkspaceInit {
    Home(Box<CodingAgentEmbeddingSnapshot>),
    Session(Box<DesktopProjection>),
}

impl NativeShell {
    pub(in crate::app) fn new(
        init: NativeShellInit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let NativeShellInit {
            runtime,
            workspace,
            projectless_workspace_selection,
            global_skills,
            preferences,
            preference_writer,
            preference_notice,
        } = init;
        let (project, projection) = match workspace {
            NativeShellWorkspaceInit::Home(project) => (*project, None),
            NativeShellWorkspaceInit::Session(projection) => {
                (projection.project().clone(), Some(*projection))
            }
        };
        assert!(
            matches!(
                &projectless_workspace_selection,
                CodingAgentWorkspaceSelection::Projectless { .. }
            ),
            "the desktop Home clear target must be a managed Projectless workspace"
        );
        let (connection, mut runtime_executor) =
            ShellConnection::connect(runtime, preference_writer);
        let runtime_shutdown_signal = runtime_executor.shutdown_signal();
        let command_tracker = CommandTracker::default();
        let center_header_focus = cx.focus_handle().tab_stop(true).tab_index(1);
        let sidebar_focus = cx.focus_handle().tab_stop(true).tab_index(2);
        let center_body_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let inspector_focus = cx.focus_handle().tab_stop(true).tab_index(5);
        let authorization_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let command_palette_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let full_message_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let search_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let modal_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let conversation_pane = cx.new(|_| ConversationPane::new());
        let conversation_header = cx.new(|_| ConversationHeader::new(center_header_focus.clone()));
        let sessions_pane = cx.new(|cx| SessionsPane::new(sidebar_focus.clone(), window, cx));
        let composer_pane = cx.new(|cx| ComposerPane::new(window, cx));
        let home_pane = cx.new(|_| HomePane::new());
        let skills_pane = cx.new(|_| SkillsPane::new());
        let inspector_pane = cx.new(|cx| InspectorPane::new(inspector_focus.clone(), cx));
        let toast_host = cx.new(|cx| ToastHost::new(window, cx));
        let root_modal_host = cx.new(|cx| {
            RootModalHost::new(
                authorization_focus.clone(),
                command_palette_focus.clone(),
                full_message_focus.clone(),
                search_focus.clone(),
                modal_focus.clone(),
                window,
                cx,
            )
        });
        let center_drawer_host =
            cx.new(|_| CenterDrawerHost::new(sessions_pane.clone(), inspector_pane.clone()));

        let subscriptions = vec![
            cx.on_focus(&center_header_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::CenterHeader, window, cx);
            }),
            cx.on_focus(&sidebar_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Sidebar, window, cx);
            }),
            cx.on_focus(&center_body_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::CenterBody, window, cx);
            }),
            cx.on_focus(&inspector_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Inspector, window, cx);
            }),
            cx.subscribe_in(
                &conversation_pane,
                window,
                |this, _, event: &ConversationPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &conversation_header,
                window,
                |this, _, event: &ConversationHeaderEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &sessions_pane,
                window,
                |this, _, event: &SessionsPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &composer_pane,
                window,
                |this, _, event: &ComposerPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &inspector_pane,
                window,
                |this, _, event: &InspectorPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &root_modal_host,
                window,
                |this, _, event: &RootModalHostEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &center_drawer_host,
                window,
                |this, _, event: &CenterDrawerHostEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.observe_window_bounds(window, Self::window_bounds_changed),
            cx.on_release(move |_, _| runtime_shutdown_signal.signal()),
        ];

        let composer_focus = composer_pane.read(cx).focus_handle().clone();
        composer_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            while let Some(updates) = runtime_executor.next_update_batch().await {
                let Some(this) = this.upgrade() else {
                    break;
                };
                this.update(cx, |this, cx| {
                    this.connection.enqueue_runtime_updates(updates);
                    let poll = this.poll_runtime();
                    this.apply_runtime_poll(poll, cx)
                });
            }
            let _ = runtime_executor.shutdown().await;
        })
        .detach();

        let thinking_selection = projection
            .as_ref()
            .map(|projection| {
                preferences.thinking_level_for_session(&projection.snapshot().session.session_id)
            })
            .unwrap_or_default();
        let home_project = project.clone();
        let active_session_id = projection.as_ref().map(|projection| {
            SessionId::from_dto(projection.snapshot().session.session_id.clone())
        });
        let initial_workspace = if projection.is_none() {
            build_session_workspace(
                project,
                projection,
                preference_notice,
                thinking_selection,
                projectless_workspace_selection.clone(),
            )
        } else {
            session_workspace_with_thinking(
                project,
                projection,
                preference_notice,
                thinking_selection,
            )
        };
        let workspace_store = match active_session_id {
            Some(session_id) => {
                let mut store = WorkspaceStore::new(build_session_workspace(
                    home_project.clone(),
                    None,
                    None,
                    DesktopThinkingLevel::Default,
                    projectless_workspace_selection.clone(),
                ));
                store.insert_session(session_id.clone(), initial_workspace);
                assert!(store.activate(&WorkspaceKey::Session(session_id)));
                store
            }
            None => WorkspaceStore::new(initial_workspace),
        };
        let app = DesktopState::new_with_workspace_defaults(
            workspace_store,
            command_tracker,
            ProjectCatalogController::default(),
            preferences,
            RuntimeWorkspaceDefaults {
                home_project,
                projectless_selection: projectless_workspace_selection,
            },
        );
        let mut shell = Self {
            connection,
            app,
            global_skills,
            views: ShellViews::new(
                conversation_pane,
                conversation_header,
                sessions_pane,
                composer_pane,
                home_pane,
                skills_pane,
                inspector_pane,
                toast_host,
                root_modal_host,
                center_drawer_host,
                subscriptions,
            ),
            ui: ShellUiState::new(
                center_header_focus,
                sidebar_focus,
                center_body_focus,
                inspector_focus,
                authorization_focus,
                command_palette_focus,
                full_message_focus,
                search_focus,
                modal_focus,
            ),
        };
        debug_assert!(shell.views.subscription_count() > 0);
        shell.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Conversation,
                UiRegion::ConversationHeader,
                UiRegion::Composer,
                UiRegion::Sessions,
                UiRegion::Inspector,
                UiRegion::Skills,
                UiRegion::Modal,
                UiRegion::Drawer,
                UiRegion::Toast,
            ]),
            cx,
        );
        shell
    }
    #[cfg(feature = "desktop-devtools")]
    pub(in crate::app) fn install_native_visual_catalog_fixture(
        &mut self,
        fixture: NativeVisualCatalogFixture,
        cx: &mut Context<Self>,
    ) {
        self.app.catalog = ProjectCatalogController::default();
        match fixture {
            NativeVisualCatalogFixture::NotLoaded => {}
            NativeVisualCatalogFixture::Loading => self.app.catalog.begin_refresh(),
            NativeVisualCatalogFixture::Ready => {
                let Some(projection) = self.app.workspaces.active_mut().projection.as_ref() else {
                    return;
                };
                let session_id = projection.snapshot().session.session_id.clone();
                let mut entry = desktop::runtime::DesktopSessionCatalogEntry {
                    session_id,
                    name: Some("Current desktop task".into()),
                    // A future timestamp is clamped to zero elapsed time by the
                    // presentation helper, keeping the replay's `now` label
                    // deterministic across calendar dates.
                    created_at: "9999-12-31T23:59:59Z".into(),
                    updated_at: "9999-12-31T23:59:59Z".into(),
                    ..Default::default()
                };
                if let Some(workspace) = self.app.workspaces.active_mut().project.workspace.as_ref()
                {
                    entry.workspace = workspace.overview.clone();
                    entry.workspace_migration =
                        coding_agent::api::view::CodingAgentWorkspaceMigration {
                            outcome: coding_agent::api::view::CodingAgentWorkspaceMigrationOutcome::NotRequired,
                            diagnostic: None,
                        };
                }
                self.app.catalog.replace_catalog(vec![entry], 0);
            }
            NativeVisualCatalogFixture::Error => self
                .app
                .catalog
                .fail_refresh("The project catalog could not be loaded."),
            NativeVisualCatalogFixture::Empty => {
                self.app.catalog.replace_catalog(Vec::new(), 0);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(in crate::app) fn install_native_visual_drawer_fixture(
        &mut self,
        fixture: NativeVisualDrawerFixture,
        cx: &mut Context<Self>,
    ) {
        self.ui.active_drawer = Some(match fixture {
            NativeVisualDrawerFixture::Sessions => CenterDrawerKind::Sessions,
            NativeVisualDrawerFixture::Inspector => CenterDrawerKind::Inspector,
        });
        self.ui.drawer_restore_focus = Some(self.ui.focus.active());
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Drawer), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(in crate::app) fn install_native_visual_home_project_fixture(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(self.app.workspaces.active_mut().projection.is_none());
        self.app.workspaces.active_mut().draft_workspace_selection =
            CodingAgentWorkspaceSelection::project(path);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(in crate::app) fn install_native_visual_non_reasoning_fixture(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(self.app.workspaces.active_mut().projection.is_none());
        self.app.workspaces.active_mut().project.selected_model_id = "review-fixture".into();
        let selected_model_id = self
            .app
            .workspaces
            .active_mut()
            .project
            .selected_model_id
            .clone();
        for model in &mut self.app.workspaces.active_mut().project.models {
            model.selected = model.id == selected_model_id;
        }
        self.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::High;
        self.reconcile_thinking_selection_with_project();
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        cx.notify();
    }
}

#[path = "../ui/conversation/adapter.rs"]
mod conversation_adapter;
#[path = "../ui/conversation/layout_adapter.rs"]
mod conversation_layout_adapter;
#[path = "../ui/shell/layout_adapter.rs"]
mod layout_adapter;
#[path = "../ui/shell/overlay_adapter.rs"]
mod overlay_adapter;
mod platform_update;
#[path = "../ui/sessions/catalog_adapter.rs"]
mod project_catalog_controller;
mod review_adapter;
mod root_actions;
mod root_view;
mod runtime_adapter;
#[cfg(test)]
mod tests;

use crate::ui::conversation::composer_pane::{ComposerPane, ComposerPaneEvent};
use crate::ui::conversation::controller::{
    ConversationSource, RESIZE_DEBOUNCE as CONVERSATION_RESIZE_DEBOUNCE,
    message_block_id as message_conversation_block_id, tool_block_id as tool_conversation_block_id,
};
use crate::ui::conversation::header::{ConversationHeader, ConversationHeaderEvent};
use crate::ui::conversation::pane::{ConversationPane, ConversationPaneEvent};
use crate::ui::conversation::{
    composer_pane, header as conversation_header, pane as conversation_pane,
};
use crate::ui::home::HomePane;
use crate::ui::inspector::pane as inspector_pane;
use crate::ui::inspector::pane::{InspectorPane, InspectorPaneEvent};
use crate::ui::sessions::pane as sessions_pane;
use crate::ui::sessions::pane::{SessionsPane, SessionsPaneEvent};
use crate::ui::shell::CenterSurface;
use crate::ui::shell::drawer::{CenterDrawerHost, CenterDrawerHostEvent, CenterDrawerKind};
use crate::ui::shell::modal::{RootModalHost, RootModalHostEvent};
use crate::ui::shell::toast::{ToastHost, ToastNotice};
use crate::ui::shell::{drawer as center_drawer_host, modal as root_modal_host};
use crate::ui::skills as skills_pane;
use crate::ui::skills::SkillsPane;

mod command_adapter;
mod commands;

fn focus_target_label(target: FocusTarget) -> &'static str {
    match target {
        FocusTarget::CenterHeader => "Center header",
        FocusTarget::Sidebar => "Sidebar",
        FocusTarget::CenterBody => "Center workspace",
        FocusTarget::Composer => "Composer",
        FocusTarget::Inspector => "Inspector",
        FocusTarget::Modal => "Modal",
    }
}
