use std::time::{Duration, Instant};

use desktop::runtime::DesktopSessionCatalogEntry;

use super::*;

pub(super) const SESSION_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub(super) struct SessionController {
    catalog: Vec<DesktopSessionCatalogEntry>,
    omitted: usize,
    refresh_deadline: Option<Instant>,
}

impl SessionController {
    pub(super) fn catalog(&self) -> &[DesktopSessionCatalogEntry] {
        &self.catalog
    }

    pub(super) fn omitted(&self) -> usize {
        self.omitted
    }

    pub(super) fn replace_catalog(
        &mut self,
        catalog: Vec<DesktopSessionCatalogEntry>,
        omitted: usize,
    ) {
        self.catalog = catalog;
        self.omitted = omitted;
    }

    pub(super) fn schedule_refresh(&mut self, now: Instant) -> Option<Instant> {
        let deadline = now + SESSION_CATALOG_REFRESH_INTERVAL;
        if self
            .refresh_deadline
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return None;
        }
        self.refresh_deadline = Some(deadline);
        Some(deadline)
    }

    pub(super) fn take_scheduled_refresh(&mut self, deadline: Instant) -> bool {
        if self.refresh_deadline != Some(deadline) {
            return false;
        }
        self.refresh_deadline = None;
        true
    }

    pub(super) fn next_session_id(&self, active_session_id: &str) -> Option<String> {
        let current = self
            .catalog
            .iter()
            .position(|session| session.session_id == active_session_id);
        let next = current.map_or(0, |index| (index + 1) % self.catalog.len());
        self.catalog
            .get(next)
            .map(|session| session.session_id.clone())
    }
}

impl NativeShell {
    pub(super) fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.set_preference_notice(format!(
                "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
            ));
            self.notify_sessions_pane(cx);
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self.composer.submitted().is_some()
            || self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            return;
        }
        let intent = DesktopCommandIntent::CreateSession;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_create_session(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => self.set_preference_notice("Creating a new session…".into()),
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn request_session_catalog(&mut self, cx: &mut Context<Self>) {
        if self
            .command_ledger
            .contains(&DesktopCommandIntent::ListSessions)
        {
            return;
        }
        let intent = DesktopCommandIntent::ListSessions;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_list_sessions(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.command_ledger.complete(command_id, &intent);
            self.set_preference_notice(message);
            self.schedule_session_catalog_refresh(cx);
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn schedule_session_catalog_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.session_controller.schedule_refresh(Instant::now()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SESSION_CATALOG_REFRESH_INTERVAL)
                .await;
            let _ =
                this.update(cx, |this, cx| {
                    if this.session_controller.take_scheduled_refresh(deadline) {
                        if this.projection.as_ref().is_none_or(|projection| {
                            projection.snapshot().active_operation.is_none()
                        }) {
                            this.request_session_catalog(cx);
                        } else {
                            this.schedule_session_catalog_refresh(cx);
                        }
                    }
                });
        })
        .detach();
    }

    pub(super) fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let already_open = self.active_workspace.session_id() == session_id
            || self.workspaces.contains_key(&session_id);
        if !already_open && self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.set_preference_notice(format!(
                "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
            ));
            self.notify_sessions_pane(cx);
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self.composer.submitted().is_some()
            || self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            self.set_preference_notice(
                "Session switching is available only while the runtime is idle.".into(),
            );
            cx.notify();
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| session_id == projection.snapshot().session.session_id)
        {
            self.set_preference_notice("The requested session is already active.".into());
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::OpenSession {
            session_id: session_id.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_open_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.set_preference_notice(format!(
                    "Opening session {}…",
                    truncate_label(&session_id, 32)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn switch_next_session(&mut self, cx: &mut Context<Self>) {
        let active = self
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.as_str())
            .unwrap_or(HOME_COMPOSER_SESSION_KEY);
        let Some(session_id) = self.session_controller.next_session_id(active) else {
            self.set_preference_notice("Loading the session catalog…".into());
            self.request_session_catalog(cx);
            return;
        };
        if session_id == active {
            self.set_preference_notice("No other project session is available.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        self.open_session(session_id, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_refresh_has_one_deadline_and_keeps_recent_order() {
        let now = Instant::now();
        let mut controller = SessionController::default();
        let deadline = controller.schedule_refresh(now).unwrap();
        assert_eq!(deadline, now + SESSION_CATALOG_REFRESH_INTERVAL);
        assert_eq!(controller.schedule_refresh(now), None);
        assert!(!controller.take_scheduled_refresh(deadline + Duration::from_secs(1)));
        assert!(controller.take_scheduled_refresh(deadline));

        controller.replace_catalog(
            vec![
                DesktopSessionCatalogEntry {
                    session_id: "session-a".into(),
                    updated_at: "2026-07-28T12:00:00Z".into(),
                    ..Default::default()
                },
                DesktopSessionCatalogEntry {
                    session_id: "session-b".into(),
                    updated_at: "2026-07-28T11:00:00Z".into(),
                    ..Default::default()
                },
            ],
            3,
        );
        assert_eq!(controller.omitted(), 3);
        assert_eq!(
            controller.next_session_id("session-a").as_deref(),
            Some("session-b")
        );
        assert_eq!(
            controller.next_session_id("missing").as_deref(),
            Some("session-a")
        );
    }
}
