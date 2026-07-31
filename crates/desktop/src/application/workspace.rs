use std::collections::HashMap;

/// Desktop-owned durable session identity.
///
/// Product DTO strings are converted at the runtime/application boundary so
/// the Home surface can never alias a real session, including one named
/// `"home"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SessionId(String);

impl SessionId {
    pub(crate) fn from_dto(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum WorkspaceKey {
    Home,
    Session(SessionId),
}

impl WorkspaceKey {
    pub(crate) fn session(value: impl Into<String>) -> Self {
        Self::Session(SessionId::from_dto(value))
    }

    pub(crate) fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Home => None,
            Self::Session(session_id) => Some(session_id),
        }
    }
}

/// Stable workspace ownership: entries never move when the active key changes.
pub(crate) struct WorkspaceStore<T> {
    active: WorkspaceKey,
    entries: HashMap<WorkspaceKey, T>,
}

impl<T> WorkspaceStore<T> {
    pub(crate) fn new(home: T) -> Self {
        Self {
            active: WorkspaceKey::Home,
            entries: HashMap::from([(WorkspaceKey::Home, home)]),
        }
    }

    pub(crate) fn active_key(&self) -> &WorkspaceKey {
        &self.active
    }

    pub(crate) fn active(&self) -> &T {
        self.entries
            .get(&self.active)
            .expect("active workspace key must reference a store entry")
    }

    pub(crate) fn active_mut(&mut self) -> &mut T {
        self.entries
            .get_mut(&self.active)
            .expect("active workspace key must reference a store entry")
    }

    pub(crate) fn get(&self, key: &WorkspaceKey) -> Option<&T> {
        self.entries.get(key)
    }

    pub(crate) fn get_mut(&mut self, key: &WorkspaceKey) -> Option<&mut T> {
        self.entries.get_mut(key)
    }

    pub(crate) fn contains(&self, key: &WorkspaceKey) -> bool {
        self.entries.contains_key(key)
    }

    pub(crate) fn activate(&mut self, key: &WorkspaceKey) -> bool {
        if !self.entries.contains_key(key) {
            return false;
        }
        self.active = key.clone();
        true
    }

    pub(crate) fn insert_session(&mut self, session_id: SessionId, workspace: T) -> Option<T> {
        self.entries
            .insert(WorkspaceKey::Session(session_id), workspace)
    }

    /// Promote the current Home state into a durable session while installing
    /// a fresh Home entry. This preserves the submitted draft's owner without
    /// allowing Home to disappear from the store.
    pub(crate) fn promote_home(&mut self, session_id: SessionId, fresh_home: T) -> Result<(), T> {
        let session_key = WorkspaceKey::Session(session_id);
        if self.active != WorkspaceKey::Home || self.entries.contains_key(&session_key) {
            return Err(fresh_home);
        }
        let promoted = self
            .entries
            .insert(WorkspaceKey::Home, fresh_home)
            .expect("Home entry must always exist");
        self.entries.insert(session_key.clone(), promoted);
        self.active = session_key;
        Ok(())
    }

    pub(crate) fn remove_session(&mut self, session_id: &SessionId) -> Option<T> {
        let key = WorkspaceKey::Session(session_id.clone());
        let removed = self.entries.remove(&key);
        if removed.is_some() && self.active == key {
            self.active = WorkspaceKey::Home;
        }
        removed
    }

    pub(crate) fn session_count(&self) -> usize {
        self.entries.len().saturating_sub(1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&WorkspaceKey, &T)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionId, WorkspaceKey, WorkspaceStore};

    #[test]
    fn real_home_session_id_never_aliases_the_home_surface() {
        let mut store = WorkspaceStore::new("home surface");
        let real_home = SessionId::from_dto("home");
        assert!(
            store
                .insert_session(real_home.clone(), "durable home")
                .is_none()
        );
        assert!(store.activate(&WorkspaceKey::Session(real_home)));
        assert_eq!(store.active(), &"durable home");
        assert_eq!(store.get(&WorkspaceKey::Home), Some(&"home surface"));
    }

    #[test]
    fn closing_active_session_falls_back_to_home_deterministically() {
        let mut store = WorkspaceStore::new("home");
        let session = SessionId::from_dto("session-a");
        store.insert_session(session.clone(), "session");
        assert!(store.activate(&WorkspaceKey::Session(session.clone())));
        assert_eq!(store.remove_session(&session), Some("session"));
        assert_eq!(store.active_key(), &WorkspaceKey::Home);
        assert_eq!(store.active(), &"home");
    }

    #[test]
    fn closing_background_session_preserves_the_active_owner() {
        let mut store = WorkspaceStore::new("home");
        let active = SessionId::from_dto("session-active");
        let background = SessionId::from_dto("session-background");
        store.insert_session(active.clone(), "active");
        store.insert_session(background.clone(), "background");
        assert!(store.activate(&WorkspaceKey::Session(active.clone())));

        assert_eq!(store.remove_session(&background), Some("background"));
        assert_eq!(store.active_key(), &WorkspaceKey::Session(active));
        assert_eq!(store.active(), &"active");
        assert_eq!(store.get(&WorkspaceKey::Home), Some(&"home"));
    }

    #[test]
    fn promoting_home_preserves_its_state_and_installs_a_fresh_home() {
        let mut store = WorkspaceStore::new("submitted draft");
        let session = SessionId::from_dto("session-created");
        assert!(store.promote_home(session.clone(), "fresh home").is_ok());
        assert_eq!(store.active(), &"submitted draft");
        assert_eq!(store.get(&WorkspaceKey::Home), Some(&"fresh home"));
        assert_eq!(
            store.get(&WorkspaceKey::Session(session)),
            Some(&"submitted draft")
        );
    }
}
