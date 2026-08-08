use crate::app::bootstrap::SessionMode;
use crate::app::session::{
    CodingAgentSessionBootstrap, CodingAgentSessionQuery, runtime_session_root,
};
use crate::runtime::facade::{
    CodingAgentPublicError, CodingAgentSession, CodingAgentSessionOptions,
    CodingAgentSessionSummary, CodingSessionError,
};

use super::CodingAgentEmbeddingContext;

impl CodingAgentEmbeddingContext {
    pub fn session_options(&self) -> Result<CodingAgentSessionOptions, CodingAgentPublicError> {
        self.session_options_internal()
            .map_err(CodingAgentPublicError::from)
    }

    /// Build the bounded durable-session navigation port for this context.
    ///
    /// Repository roots and session directories remain private inside the
    /// returned handle; adapters address sessions only by product identity.
    pub fn session_query(&self) -> Result<CodingAgentSessionQuery, CodingAgentPublicError> {
        CodingAgentSessionQuery::from_run_options(&self.resolved.session)
            .map_err(CodingAgentPublicError::from)
    }

    /// Build a durable-session directory query across every workspace stored
    /// under this context's session root.
    ///
    /// Unlike [`Self::session_query`], this keeps the configured repository
    /// root but removes the current workspace/cwd filter. Product surfaces that
    /// group historical sessions by project should use this directory view.
    pub fn session_directory_query(
        &self,
    ) -> Result<CodingAgentSessionQuery, CodingAgentPublicError> {
        CodingAgentSessionQuery::from_run_options_unscoped(&self.resolved.session)
            .map_err(CodingAgentPublicError::from)
    }

    /// Build an opaque session bootstrap handle for this context.
    pub fn session_bootstrap(&self) -> CodingAgentSessionBootstrap {
        CodingAgentSessionBootstrap::from_internal(
            self.resolved.session.clone(),
            None,
            self.resolved.session_name.clone(),
            self.options.default_agent_profile_id.clone(),
            self.options.tool_authorization_mode,
        )
    }

    pub(crate) fn session_options_internal(
        &self,
    ) -> Result<CodingAgentSessionOptions, CodingSessionError> {
        let options = match self.options.workspace.as_ref() {
            Some(workspace) => {
                CodingAgentSessionOptions::new().with_resolved_workspace(workspace.clone())
            }
            None => CodingAgentSessionOptions::new().with_cwd(self.options.cwd.clone()),
        };
        let mut options = options
            .with_default_agent_profile_id(self.options.default_agent_profile_id.clone())
            .with_tool_authorization_mode(self.options.tool_authorization_mode);
        if let Some(root) = self
            .resolved
            .session
            .as_ref()
            .map(runtime_session_root)
            .transpose()?
            .flatten()
        {
            options = options.with_session_log_root(root);
        }
        if let Some(name) = self.resolved.session_name.as_deref() {
            options = options.with_session_name(name);
        }
        Ok(options)
    }

    pub fn list_sessions(&self) -> Result<Vec<CodingAgentSessionSummary>, CodingAgentPublicError> {
        self.list_sessions_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn list_sessions_internal(
        &self,
    ) -> Result<Vec<CodingAgentSessionSummary>, CodingSessionError> {
        if !self.sessions_are_persistent() {
            return Ok(Vec::new());
        }
        CodingAgentSession::list_internal(self.session_options_internal()?)
    }

    pub async fn create_session(&self) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.create_session_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    /// Create a new persistent session with a caller-assigned product id.
    ///
    /// The id is normalized and validated by the product session repository.
    /// This is create-only: an existing id returns a typed session error rather
    /// than opening or replacing the existing session.
    pub async fn create_session_with_id(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.create_session_with_id_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn create_session_internal(
        &self,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        let options = self.session_options_internal()?;
        if self.sessions_are_persistent() {
            CodingAgentSession::create_internal(options).await
        } else {
            CodingAgentSession::non_persistent_internal(options).await
        }
    }

    pub(crate) async fn create_session_with_id_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::create_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    pub async fn open_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.open_session_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_session_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::open_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    pub async fn open_or_create_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.open_or_create_session_internal(session_id)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_or_create_session_internal(
        &self,
        session_id: impl Into<String>,
    ) -> Result<CodingAgentSession, CodingSessionError> {
        self.require_persistent_sessions()?;
        CodingAgentSession::open_or_create_internal(
            self.session_options_internal()?
                .with_session_id(session_id.into()),
        )
        .await
    }

    fn sessions_are_persistent(&self) -> bool {
        self.resolved
            .session
            .as_ref()
            .is_some_and(|session| matches!(session.mode, SessionMode::Enabled))
    }

    fn require_persistent_sessions(&self) -> Result<(), CodingSessionError> {
        if self.sessions_are_persistent() {
            Ok(())
        } else {
            Err(CodingSessionError::UnsupportedCapability {
                capability: "opening a named session while persistence is disabled".into(),
            })
        }
    }
}
