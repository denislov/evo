use super::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH, CenterDrawerKind,
    Context, FocusInputModality, FocusTarget, KeyDownEvent, MIN_CONVERSATION_WIDTH, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, NativeShell, PanelResizeState, PanelVisibility, PreferencePanel,
    PreferencesIntent, ResizablePanel, SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH,
    SESSION_PANEL_WIDTH, ShellLayout, UiChangeSet, UiRegion, Window, WindowBounds,
};

impl NativeShell {
    pub(super) fn apply_panel_width(
        &mut self,
        panel: ResizablePanel,
        width: u32,
        cx: &mut Context<Self>,
    ) {
        let preference_panel = match panel {
            ResizablePanel::Sessions => PreferencePanel::Sessions,
            ResizablePanel::Context => PreferencePanel::Context,
        };
        let transition = self.connection.controller.reduce(
            &mut self.app,
            super::DesktopEvent::Preferences(PreferencesIntent::SetPanelWidth {
                panel: preference_panel,
                width,
            }),
            |state, event| {
                let super::DesktopEvent::Preferences(PreferencesIntent::SetPanelWidth {
                    panel,
                    width,
                }) = event
                else {
                    unreachable!("panel resize receives one typed preferences intent")
                };
                let changed = match panel {
                    PreferencePanel::Sessions
                        if state.preferences.sessions_panel_width != width =>
                    {
                        state.preferences.sessions_panel_width = width;
                        true
                    }
                    PreferencePanel::Context if state.preferences.context_panel_width != width => {
                        state.preferences.context_panel_width = width;
                        true
                    }
                    _ => false,
                };
                if !changed {
                    return super::Transition::default();
                }
                let region = match panel {
                    PreferencePanel::Sessions => UiRegion::Sessions,
                    PreferencePanel::Context => UiRegion::Inspector,
                };
                super::Transition::from_changes(UiChangeSet::from_regions(&[
                    UiRegion::Root,
                    region,
                    UiRegion::ConversationHeader,
                ]))
            },
        );
        self.apply_transition(transition, cx);
    }

    pub(super) fn visibility(&self) -> PanelVisibility {
        PanelVisibility {
            sessions: self.app.preferences.sessions_panel_visible,
            context: self.app.preferences.context_panel_visible,
        }
    }

    pub(super) fn layout(&self, window: &Window) -> ShellLayout {
        let viewport = window.viewport_size();
        self.resolve_layout(
            u32::from(viewport.width),
            u32::from(viewport.height),
            self.visibility(),
        )
    }

    pub(super) fn resolve_layout(
        &self,
        width: u32,
        height: u32,
        visibility: PanelVisibility,
    ) -> ShellLayout {
        ShellLayout::resolve_with_panel_widths(
            width,
            height,
            visibility,
            self.app.preferences.sessions_panel_width,
            self.app.preferences.context_panel_width,
        )
    }

    pub(super) fn begin_panel_resize(
        &mut self,
        panel: ResizablePanel,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.ui.panel_resize = None;
            match panel {
                ResizablePanel::Sessions => {
                    self.apply_panel_width(panel, SESSION_PANEL_WIDTH, cx);
                }
                ResizablePanel::Context => {
                    self.apply_panel_width(panel, CONTEXT_PANEL_WIDTH, cx);
                }
            }
            self.schedule_preferences();
            return;
        }

        self.ui.panel_resize = Some(PanelResizeState {
            panel,
            pointer_origin_x: f32::from(event.position.x),
            width_origin: match panel {
                ResizablePanel::Sessions => self.app.preferences.sessions_panel_width,
                ResizablePanel::Context => self.app.preferences.context_panel_width,
            },
        });
    }

    pub(super) fn update_panel_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.ui.panel_resize else {
            return;
        };
        let delta = f32::from(event.position.x) - resize.pointer_origin_x;
        let desired = match resize.panel {
            ResizablePanel::Sessions => resize.width_origin as f32 + delta,
            ResizablePanel::Context => resize.width_origin as f32 - delta,
        };
        let layout = self.layout(window);
        let (minimum, configured_maximum, other_width) = match resize.panel {
            ResizablePanel::Sessions => (
                SESSION_PANEL_MIN_WIDTH,
                SESSION_PANEL_MAX_WIDTH,
                layout.inspector.map_or(0, |bounds| bounds.width),
            ),
            ResizablePanel::Context => (
                CONTEXT_PANEL_MIN_WIDTH,
                CONTEXT_PANEL_MAX_WIDTH,
                layout.sidebar.map_or(0, |bounds| bounds.width),
            ),
        };
        let viewport_width = u32::from(window.viewport_size().width);
        let maximum = configured_maximum.min(
            viewport_width
                .saturating_sub(MIN_CONVERSATION_WIDTH)
                .saturating_sub(other_width)
                .max(minimum),
        );
        let width = (desired.round() as i64).clamp(i64::from(minimum), i64::from(maximum)) as u32;

        self.apply_panel_width(resize.panel, width, cx);
    }

    pub(super) fn finish_panel_resize(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui.panel_resize.take().is_some() {
            self.schedule_preferences();
            self.flush_queued_effects(cx);
        }
    }

    pub(super) fn set_focus_input_modality(
        &mut self,
        modality: FocusInputModality,
        cx: &mut Context<Self>,
    ) {
        if self.ui.focus_input_modality == modality {
            return;
        }
        self.ui.focus_input_modality = modality;
        self.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Sessions,
                UiRegion::ConversationHeader,
                UiRegion::Composer,
                UiRegion::Inspector,
                UiRegion::Toast,
            ]),
            cx,
        );
        cx.notify();
    }

    pub(super) fn note_pointer_input(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_focus_input_modality(FocusInputModality::Pointer, cx);
    }

    pub(super) fn note_keyboard_input(
        &mut self,
        _: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_focus_input_modality(FocusInputModality::Keyboard, cx);
    }

    pub(super) fn record_focus(
        &mut self,
        target: FocusTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = self.layout(window);
        let previous = self.ui.focus.active();
        if self.ui.focus.request(target, layout) {
            if self.ui.active_drawer.is_some() {
                self.ui.drawer_restore_focus = Some(target);
            }
            cx.notify();
        }
        let mut changes = UiChangeSet::default();
        if previous == FocusTarget::Sidebar || target == FocusTarget::Sidebar {
            changes.insert(UiRegion::Sessions);
        }
        if previous == FocusTarget::CenterHeader || target == FocusTarget::CenterHeader {
            changes.insert(UiRegion::ConversationHeader);
        }
        if previous == FocusTarget::Composer || target == FocusTarget::Composer {
            changes.insert(UiRegion::Composer);
        }
        if previous == FocusTarget::Inspector || target == FocusTarget::Inspector {
            changes.insert(UiRegion::Inspector);
        }
        self.refresh_views(changes, cx);
    }

    pub(super) fn window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.window_bounds();
        let restore = bounds.get_bounds();
        self.app.preferences.window.x = f32::from(restore.origin.x).round() as i32;
        self.app.preferences.window.y = f32::from(restore.origin.y).round() as i32;
        self.app.preferences.window.width = u32::from(restore.size.width);
        self.app.preferences.window.height = u32::from(restore.size.height);
        self.app.preferences.window.maximized = matches!(bounds, WindowBounds::Maximized(_));

        let viewport = window.viewport_size();
        let forced_layout = self.resolve_layout(
            u32::from(viewport.width),
            u32::from(viewport.height),
            PanelVisibility::default(),
        );
        let drawer_became_dockable = match self.ui.active_drawer {
            Some(CenterDrawerKind::Sessions) if forced_layout.sidebar.is_some() => {
                self.app.preferences.sessions_panel_visible = true;
                true
            }
            Some(CenterDrawerKind::Inspector) if forced_layout.inspector.is_some() => {
                self.app.preferences.context_panel_visible = true;
                true
            }
            _ => false,
        };
        if drawer_became_dockable {
            self.dismiss_drawer(window, cx, true);
        }
        let layout = self.layout(window);
        let previous_focus = self.ui.focus.active();
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() != previous_focus {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Inspector,
                UiRegion::ConversationHeader,
                UiRegion::Drawer,
            ]),
            cx,
        );
        cx.notify();
    }
}
