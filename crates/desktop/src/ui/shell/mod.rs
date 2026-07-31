pub(crate) mod drawer;
mod layout;
pub(crate) mod modal;
pub(crate) mod presentation;
mod state;
pub(crate) mod toast;

use std::collections::VecDeque;

use gpui::{Entity, Subscription};

use self::{drawer::CenterDrawerHost, modal::RootModalHost, toast::ToastHost};
use crate::application::effect::DesktopEffect;
use crate::application::reducer::DesktopController;
use crate::platform::preferences::PreferenceWriter;
use crate::runtime::{
    DesktopRuntimeBridge, DesktopRuntimeEventStream, DesktopRuntimeShutdownError,
    DesktopRuntimeShutdownGuard, DesktopRuntimeShutdownSignal, DesktopRuntimeUpdate,
    RuntimeCommandClient,
};
use crate::ui::conversation::{
    composer_pane::ComposerPane, header::ConversationHeader, pane::ConversationPane,
};
use crate::ui::inspector::pane::InspectorPane;
use crate::ui::sessions::pane::SessionsPane;
use crate::ui::{home::HomePane, skills::SkillsPane};

pub(crate) use layout::*;
pub(crate) use state::{CenterNavigationTarget, CenterSurface, ShellUiState};

/// Child views and the subscriptions that keep their event wiring alive.
///
/// A constructed shell must own its entire child tree and at least the root
/// lifecycle subscription. Keeping both here prevents an entity from outliving
/// the event wiring that makes it interactive.
pub(crate) struct ShellViews {
    pub(crate) conversation_pane: Entity<ConversationPane>,
    pub(crate) conversation_header: Entity<ConversationHeader>,
    pub(crate) sessions_pane: Entity<SessionsPane>,
    pub(crate) composer_pane: Entity<ComposerPane>,
    pub(crate) home_pane: Entity<HomePane>,
    pub(crate) skills_pane: Entity<SkillsPane>,
    pub(crate) inspector_pane: Entity<InspectorPane>,
    pub(crate) toast_host: Entity<ToastHost>,
    pub(crate) root_modal_host: Entity<RootModalHost>,
    pub(crate) center_drawer_host: Entity<CenterDrawerHost>,
    subscriptions: Vec<Subscription>,
}

impl ShellViews {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conversation_pane: Entity<ConversationPane>,
        conversation_header: Entity<ConversationHeader>,
        sessions_pane: Entity<SessionsPane>,
        composer_pane: Entity<ComposerPane>,
        home_pane: Entity<HomePane>,
        skills_pane: Entity<SkillsPane>,
        inspector_pane: Entity<InspectorPane>,
        toast_host: Entity<ToastHost>,
        root_modal_host: Entity<RootModalHost>,
        center_drawer_host: Entity<CenterDrawerHost>,
        subscriptions: Vec<Subscription>,
    ) -> Self {
        assert!(
            !subscriptions.is_empty(),
            "native shell view wiring must own its lifecycle subscriptions"
        );
        Self {
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
        }
    }

    pub(crate) fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }
}

/// Runtime connection, executor, and effect admission owned by the shell.
///
/// Dropping this value severs command admission before the view tree is
/// released. Runtime shutdown itself remains explicit through the root release
/// subscription installed during composition.
pub(crate) struct ShellConnection {
    pub(crate) runtime_client: Option<RuntimeCommandClient>,
    pub(crate) runtime_updates: VecDeque<DesktopRuntimeUpdate>,
    pub(crate) controller: DesktopController,
    pub(crate) queued_effects: VecDeque<DesktopEffect>,
    pub(crate) preference_writer: Option<PreferenceWriter>,
}

impl ShellConnection {
    pub(crate) fn connect(
        runtime: DesktopRuntimeBridge,
        preference_writer: Option<PreferenceWriter>,
    ) -> (Self, ShellRuntimeExecutor) {
        let (runtime_client, events, shutdown) = runtime.into_parts();
        (
            Self {
                runtime_client: Some(runtime_client),
                runtime_updates: VecDeque::new(),
                controller: DesktopController::new(),
                queued_effects: VecDeque::new(),
                preference_writer,
            },
            ShellRuntimeExecutor { events, shutdown },
        )
    }

    pub(crate) fn enqueue_runtime_updates(
        &mut self,
        updates: impl IntoIterator<Item = DesktopRuntimeUpdate>,
    ) {
        self.runtime_updates.extend(updates);
    }
}

/// Sole event-stream and shutdown owner transferred to the GPUI executor.
pub(crate) struct ShellRuntimeExecutor {
    events: DesktopRuntimeEventStream,
    shutdown: DesktopRuntimeShutdownGuard,
}

impl ShellRuntimeExecutor {
    pub(crate) fn shutdown_signal(&self) -> DesktopRuntimeShutdownSignal {
        self.shutdown.signal_handle()
    }

    pub(crate) async fn next_update_batch(&mut self) -> Option<Vec<DesktopRuntimeUpdate>> {
        self.events.next_update_batch().await
    }

    pub(crate) async fn shutdown(self) -> Result<(), DesktopRuntimeShutdownError> {
        let Self {
            mut events,
            shutdown,
        } = self;
        shutdown.shutdown(&mut events).await
    }
}
