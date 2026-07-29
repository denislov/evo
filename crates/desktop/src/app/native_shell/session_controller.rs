use desktop::runtime::{DesktopSessionCatalogEntry, MAX_DESKTOP_SESSION_CATALOG};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::*;

#[derive(Default)]
pub(super) struct SessionController {
    catalog: Vec<DesktopSessionCatalogEntry>,
    omitted: usize,
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

    pub(super) fn rename_session(
        &mut self,
        session_id: &str,
        name: Option<String>,
        updated_at: String,
    ) -> bool {
        let Some(session) = self
            .catalog
            .iter_mut()
            .find(|session| session.session_id == session_id)
        else {
            return false;
        };
        session.name = name;
        session.updated_at = updated_at;
        true
    }

    pub(super) fn insert_created_session(&mut self, entry: DesktopSessionCatalogEntry) {
        let replaced = self
            .catalog
            .iter()
            .position(|session| session.session_id == entry.session_id)
            .map(|index| self.catalog.remove(index))
            .is_some();
        self.catalog.insert(0, entry);
        if self.catalog.len() > MAX_DESKTOP_SESSION_CATALOG {
            self.catalog.truncate(MAX_DESKTOP_SESSION_CATALOG);
            if !replaced {
                self.omitted = self.omitted.saturating_add(1);
            }
        }
    }

    pub(super) fn remove_session(&mut self, session_id: &str) -> bool {
        let before = self.catalog.len();
        self.catalog
            .retain(|session| session.session_id != session_id);
        self.catalog.len() != before
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
            self.notify_toast_host(cx);
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn rename_session(
        &mut self,
        session_id: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let intent = DesktopCommandIntent::RenameSession {
            session_id: session_id.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_rename_session(command_id, &session_id, Some(&name))
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.command_ledger.complete(command_id, &intent);
            self.set_preference_notice(message);
            self.notify_toast_host(cx);
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn insert_active_session_into_catalog(&mut self) -> bool {
        let Some(projection) = self.projection.as_ref() else {
            return false;
        };
        let session_id = projection.snapshot().session.session_id.clone();
        let cwd = self
            .project
            .workspace
            .as_ref()
            .and_then(|workspace| match &workspace.scope {
                CodingAgentWorkspaceScope::Project { cwd } => Some(cwd.display().to_string()),
                CodingAgentWorkspaceScope::Projectless { .. }
                | CodingAgentWorkspaceScope::Legacy { .. } => None,
            });
        let cwd = cwd.or_else(|| {
            self.project
                .workspace
                .is_none()
                .then(|| self.project.cwd.display().to_string())
        });
        let observed_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default();
        self.session_controller
            .insert_created_session(DesktopSessionCatalogEntry {
                session_id,
                name: None,
                cwd,
                created_at: observed_at.clone(),
                updated_at: observed_at,
                active_leaf_id: None,
            });
        true
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
            self.set_preference_notice(
                "Refresh the session catalog before switching sessions.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
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
    fn catalog_local_mutations_keep_recent_order_and_bounds() {
        let mut controller = SessionController::default();
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
        controller.insert_created_session(DesktopSessionCatalogEntry {
            session_id: "session-c".into(),
            updated_at: "2026-07-30T12:00:00Z".into(),
            ..Default::default()
        });
        assert_eq!(controller.catalog()[0].session_id, "session-c");
        assert!(controller.rename_session(
            "session-c",
            Some("Created locally".into()),
            "2026-07-30T12:01:00Z".into(),
        ));
        assert_eq!(
            controller.catalog()[0].name.as_deref(),
            Some("Created locally")
        );
        assert!(controller.remove_session("session-c"));
        assert!(!controller.remove_session("session-c"));

        let mut bounded = SessionController::default();
        for index in 0..=MAX_DESKTOP_SESSION_CATALOG {
            bounded.insert_created_session(DesktopSessionCatalogEntry {
                session_id: format!("session-{index}"),
                ..Default::default()
            });
        }
        assert_eq!(bounded.catalog().len(), MAX_DESKTOP_SESSION_CATALOG);
        assert_eq!(bounded.catalog()[0].session_id, "session-128");
        assert_eq!(bounded.omitted(), 1);
    }
}
