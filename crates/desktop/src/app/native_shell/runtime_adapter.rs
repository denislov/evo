use super::{
    Arc, ClipboardItem, ComposerRunningMode, Context, DesktopController, DesktopEffect,
    DesktopEvent, DesktopPickerKind, DesktopThinkingLevel, DesktopTimer, DesktopTimerKind, Instant,
    MAX_RUNTIME_UPDATES_PER_FRAME, NativeShell, PathPromptOptions, PlatformOutcome, PlatformResult,
    PreferenceWriteResult, RuntimePoll, ToastNotice, Transition, UiChangeSet, UiRegion,
    WorkspaceKey, center_drawer_host, composer_pane, conversation_header, conversation_pane,
    inspector_pane, root_modal_host, sessions_pane, skills_pane,
};
use crate::platform::external_editor::launch_external_editor;

fn inspector_telemetry_refresh_delay(
    last_refresh: Option<Instant>,
    now: Instant,
) -> std::time::Duration {
    last_refresh.map_or(std::time::Duration::ZERO, |last_refresh| {
        super::INSPECTOR_TELEMETRY_REFRESH_INTERVAL
            .saturating_sub(now.saturating_duration_since(last_refresh))
    })
}

impl NativeShell {
    pub(super) fn with_controller<T>(
        &mut self,
        reduce: impl FnOnce(&mut DesktopController, &mut Self) -> T,
    ) -> T {
        let mut controller = std::mem::take(&mut self.connection.controller);
        let result = reduce(&mut controller, self);
        self.connection.controller = controller;
        result
    }

    pub(super) fn dispatch_platform_result(
        &mut self,
        result: PlatformResult,
        cx: &mut Context<Self>,
    ) {
        let transition = self.with_controller(|controller, this| {
            controller.reduce_async(this, DesktopEvent::Platform(result))
        });
        self.apply_transition(transition, cx);
    }

    pub(super) fn dispatch_timer(&mut self, timer: DesktopTimer, cx: &mut Context<Self>) {
        let transition = self.with_controller(|controller, this| {
            controller.reduce_async(this, DesktopEvent::Timer(timer))
        });
        self.apply_transition(transition, cx);
    }

    pub(super) fn queue_transition(&mut self, transition: Transition) {
        let (changes, effects) = transition.into_parts();
        assert!(
            changes.is_empty(),
            "queued transitions cannot hide UI changes"
        );
        self.connection.queued_effects.extend(effects);
    }

    pub(super) fn apply_transition(&mut self, transition: Transition, cx: &mut Context<Self>) {
        let (changes, effects) = transition.into_parts();
        self.refresh_views(changes, cx);
        self.connection.queued_effects.extend(effects);
        self.flush_queued_effects(cx);
    }

    pub(super) fn flush_queued_effects(&mut self, cx: &mut Context<Self>) {
        while let Some(effect) = self.connection.queued_effects.pop_front() {
            self.execute_effect(effect, cx);
        }
    }

    pub(super) fn execute_effect(&mut self, effect: DesktopEffect, cx: &mut Context<Self>) {
        match effect {
            DesktopEffect::PickPaths { identity, picker } => {
                let options = match picker {
                    DesktopPickerKind::Attachments => PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: true,
                        prompt: Some("Attach files or images".into()),
                    },
                    DesktopPickerKind::ProjectDirectory => PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some("Choose a project directory".into()),
                    },
                };
                let selection = cx.prompt_for_paths(options);
                cx.spawn(async move |this, cx| {
                    let outcome = match selection.await {
                        Ok(Ok(Some(paths))) => PlatformOutcome::Completed(paths),
                        Ok(Ok(None)) => PlatformOutcome::Cancelled,
                        Ok(Err(_)) | Err(_) => PlatformOutcome::Failed(match picker {
                            DesktopPickerKind::Attachments => {
                                "The file picker could not be opened.".into()
                            }
                            DesktopPickerKind::ProjectDirectory => {
                                "The directory picker could not be opened.".into()
                            }
                        }),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_platform_result(
                            PlatformResult::PathsPicked {
                                identity,
                                picker,
                                outcome,
                            },
                            cx,
                        );
                    });
                })
                .detach();
            }
            DesktopEffect::WriteClipboard { identity, text, .. } => {
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                self.dispatch_platform_result(
                    PlatformResult::ClipboardWritten {
                        identity,
                        outcome: PlatformOutcome::Completed(()),
                    },
                    cx,
                );
            }
            DesktopEffect::WritePreferences {
                identity,
                preferences,
            } => {
                let Some(writer) = self.connection.preference_writer.as_ref() else {
                    self.dispatch_platform_result(
                        PlatformResult::PreferencesWritten {
                            identity,
                            outcome: PlatformOutcome::Failed(
                                "Desktop preference writer is unavailable.".into(),
                            ),
                        },
                        cx,
                    );
                    return;
                };
                let completion = writer.schedule(preferences);
                cx.spawn(async move |this, cx| {
                    let outcome = match completion.await {
                        Ok(PreferenceWriteResult::Written) => PlatformOutcome::Completed(()),
                        Ok(PreferenceWriteResult::Superseded) => PlatformOutcome::Cancelled,
                        Ok(PreferenceWriteResult::Failed(message)) => {
                            PlatformOutcome::Failed(message)
                        }
                        Err(_) => PlatformOutcome::Failed(
                            "Desktop preference writer stopped before completion.".into(),
                        ),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_platform_result(
                            PlatformResult::PreferencesWritten { identity, outcome },
                            cx,
                        );
                    });
                })
                .detach();
            }
            DesktopEffect::RequestResync {
                identity,
                command_id,
            } => {
                let outcome = self.connection.runtime_client.as_ref().map_or_else(
                    || PlatformOutcome::Failed("desktop runtime is stopped".into()),
                    |runtime| match runtime.try_resync(command_id) {
                        Ok(()) => PlatformOutcome::Completed(()),
                        Err(error) => PlatformOutcome::Failed(error.to_string()),
                    },
                );
                self.dispatch_platform_result(
                    PlatformResult::ResyncRequested { identity, outcome },
                    cx,
                );
            }
            DesktopEffect::LaunchExternalEditor {
                identity,
                preference,
                target,
                ..
            } => {
                let outcome = launch_external_editor(&preference, target.path()).map_or_else(
                    |error| PlatformOutcome::Failed(error.to_string()),
                    |()| PlatformOutcome::Completed(()),
                );
                self.dispatch_platform_result(
                    PlatformResult::ExternalEditorLaunched { identity, outcome },
                    cx,
                );
            }
            DesktopEffect::ScheduleTimer { timer, delay } => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_timer(timer, cx);
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn poll_runtime(&mut self) -> RuntimePoll {
        if self.connection.runtime_client.is_none() {
            return RuntimePoll {
                transition: Transition::default(),
                running: false,
            };
        }
        let mut transition = Transition::default();
        let mut applied = 0;
        while applied < MAX_RUNTIME_UPDATES_PER_FRAME {
            let Some(update) = self.connection.runtime_updates.pop_front() else {
                break;
            };
            let reduced = self.with_controller(|controller, this| {
                controller.reduce_runtime(&mut this.app, update)
            });
            transition.merge(reduced);
            if self.app.take_runtime_preferences_dirty() {
                self.schedule_preferences();
            }
            applied += 1;
        }
        RuntimePoll {
            transition,
            running: self.app.active_runtime_is_running(),
        }
    }

    pub(super) fn apply_runtime_poll(
        &mut self,
        mut poll: RuntimePoll,
        cx: &mut Context<Self>,
    ) -> bool {
        let conversation_needs_refresh = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .needs_row_refresh();
        if conversation_needs_refresh && !self.refresh_conversation_rows_at_current_width(cx) {
            poll.transition.merge(Transition::changed(UiRegion::Root));
        }
        self.apply_transition(poll.transition, cx);
        poll.running
    }

    #[cfg(test)]
    pub(super) fn poll_runtime_for_test(&mut self, cx: &mut Context<Self>) -> bool {
        let poll = self.poll_runtime();
        self.apply_runtime_poll(poll, cx)
    }

    pub(super) fn refresh_views(&mut self, mut changes: UiChangeSet, cx: &mut Context<Self>) {
        #[cfg(test)]
        if !changes.is_empty() {
            self.ui.runtime_ui_notification_count += 1;
        }

        // Host coordination lives here so feature refresh callers only name the
        // state they changed. Sessions owns catalog notices and modal admission;
        // inspector actions can also publish notices.
        if changes.contains(UiRegion::Sessions) {
            changes.insert(UiRegion::Toast);
            changes.insert(UiRegion::Modal);
        }
        if changes.contains(UiRegion::Inspector) {
            changes.insert(UiRegion::Toast);
        }

        if changes.contains(UiRegion::Root) {
            cx.notify();
        }
        if changes.contains(UiRegion::Sessions) {
            let view_model = sessions_pane::view_model(&self.app, &self.ui);
            self.views.sessions_pane.update(cx, |pane, cx| {
                pane.set_view_model(view_model);
                cx.notify();
            });
        }
        if changes.contains(UiRegion::Composer) {
            let view_model = composer_pane::view_model(self.app.workspaces.active());
            self.views.composer_pane.update(cx, |pane, cx| {
                pane.set_view_model(view_model);
                cx.notify();
            });
        }
        if changes.contains(UiRegion::Conversation) {
            let view_model = conversation_pane::view_model(self.app.workspaces.active(), &self.ui);
            self.views.conversation_pane.update(cx, |pane, cx| {
                pane.set_view_model(view_model);
                cx.notify();
            });
        }
        if changes.contains(UiRegion::Inspector) {
            self.ui.inspector_telemetry_last_refresh = Some(Instant::now());
            self.ui.inspector_telemetry_refresh_deadline = None;
            let view_model =
                inspector_pane::view_model(&self.app, &self.ui, self.global_skills.len());
            self.views.inspector_pane.update(cx, |pane, cx| {
                pane.set_view_model(view_model);
                cx.notify();
            });
        } else if changes.contains(UiRegion::InspectorTelemetry) {
            self.schedule_inspector_telemetry_refresh(cx);
        }
        if changes.contains(UiRegion::Skills) {
            let view_model = skills_pane::view_model(&self.global_skills);
            self.views.skills_pane.update(cx, |pane, cx| {
                pane.set_view_model(view_model);
                cx.notify();
            });
        }
        if changes.contains(UiRegion::Toast) {
            let notice_owner: Arc<str> = match self.app.workspaces.active_key() {
                WorkspaceKey::Home => Arc::from("workspace:home"),
                WorkspaceKey::Session(session_id) => {
                    Arc::from(format!("session:{}", session_id.as_str()))
                }
            };
            let notice = self
                .app
                .workspaces
                .active()
                .preference_notice
                .as_ref()
                .map(|message| ToastNotice {
                    session_id: notice_owner,
                    revision: self.app.workspaces.active().preference_notice_revision,
                    message: Arc::from(message.as_str()),
                });
            self.views.toast_host.update(cx, |host, cx| {
                host.observe_notice(notice, cx);
            });
        }
        if changes.contains(UiRegion::ConversationHeader) {
            let view_model = conversation_header::view_model(&self.app, &self.ui);
            self.views
                .conversation_header
                .update(cx, |conversation_header, cx| {
                    conversation_header.set_view_model(view_model);
                    cx.notify();
                });
        }
        if changes.contains(UiRegion::Modal) {
            let view_model = root_modal_host::view_model(&self.app, &self.ui);
            self.views.root_modal_host.update(cx, |host, cx| {
                host.set_view_model(view_model);
                cx.notify();
            });
        }
        if changes.contains(UiRegion::Drawer) {
            let view_model = center_drawer_host::view_model(&self.app, &self.ui);
            self.views.center_drawer_host.update(cx, |host, cx| {
                host.set_view_model(view_model);
                cx.notify();
            });
        }
    }

    pub(super) fn active_composer_running_mode(&self) -> ComposerRunningMode {
        self.app
            .workspaces
            .active()
            .presentation
            .composer_running_mode
    }

    pub(super) fn set_active_composer_running_mode(
        &mut self,
        mode: ComposerRunningMode,
        cx: &mut Context<Self>,
    ) {
        self.app
            .workspaces
            .active_mut()
            .presentation
            .composer_running_mode = mode;
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
    }

    pub(super) fn schedule_inspector_telemetry_refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let delay =
            inspector_telemetry_refresh_delay(self.ui.inspector_telemetry_last_refresh, now);
        if delay.is_zero() {
            self.ui.inspector_telemetry_last_refresh = Some(now);
            self.ui.inspector_telemetry_refresh_deadline = None;
            self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
            return;
        }

        let deadline = now + delay;
        if self
            .ui
            .inspector_telemetry_refresh_deadline
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.ui.inspector_telemetry_refresh_deadline = Some(deadline);
        let owner = self.app.workspaces.active_key().clone();
        match self.connection.controller.schedule_timer(
            owner,
            DesktopTimerKind::InspectorTelemetryRefresh,
            delay,
        ) {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.ui.inspector_telemetry_refresh_deadline = None;
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            }
        }
    }

    pub(super) fn schedule_preferences(&mut self) {
        if self.connection.preference_writer.is_none() {
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .write_preferences(owner, self.app.preferences.clone())
        {
            Ok(transition) => self.queue_transition(transition),
            Err(error) => self
                .app
                .workspaces
                .active_mut()
                .set_preference_notice(error.to_string()),
        }
    }

    pub(super) fn remember_thinking_selection(
        &mut self,
        session_id: &str,
        selection: DesktopThinkingLevel,
    ) {
        if self
            .app
            .preferences
            .set_thinking_level_for_session(session_id, selection)
        {
            self.schedule_preferences();
        }
    }

    #[cfg(feature = "desktop-devtools")]
    pub(super) fn reconcile_thinking_selection_with_project(&mut self) {
        let owner = self.app.workspaces.active_key().clone();
        self.reconcile_thinking_selection_for(&owner);
    }
}

#[cfg(test)]
mod tests {
    use super::inspector_telemetry_refresh_delay;
    use std::time::{Duration, Instant};

    #[test]
    fn inspector_telemetry_refresh_is_immediate_then_throttled() {
        let start = Instant::now();
        assert_eq!(
            inspector_telemetry_refresh_delay(None, start),
            Duration::ZERO
        );
        assert_eq!(
            inspector_telemetry_refresh_delay(Some(start), start + Duration::from_millis(50)),
            Duration::from_millis(200)
        );
        assert_eq!(
            inspector_telemetry_refresh_delay(Some(start), start + Duration::from_millis(250)),
            Duration::ZERO
        );
    }
}
