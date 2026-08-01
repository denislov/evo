use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::profiles::ProfileId;
use crate::workspace::{
    CodingAgentWorkspaceResolutionError, CodingAgentWorkspaceScope,
    validate_persisted_project_path, validate_workspace_id,
};

pub(crate) const SESSION_SCHEMA: &str = "evo.session";
pub(crate) const LEGACY_SESSION_VERSION: u32 = 1;
pub(crate) const SESSION_VERSION: u32 = 2;
pub(crate) const EVENT_SCHEMA: &str = "evo.session.event";
pub(crate) const EVENT_VERSION: u32 = 2;
pub(crate) const SESSION_MANIFEST_FILE: &str = "session.json";
pub(crate) const SESSION_EVENT_LOG_FILE: &str = "events.jsonl";
pub(crate) const SESSION_OUTBOX_LOG_FILE: &str = "outbox.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum PersistedWorkspaceScope {
    Project { cwd: String },
    Projectless { workspace_id: String },
}

impl PersistedWorkspaceScope {
    pub(crate) fn from_product(
        scope: &CodingAgentWorkspaceScope,
    ) -> Result<Self, CodingAgentWorkspaceResolutionError> {
        match scope {
            CodingAgentWorkspaceScope::Project { cwd } => {
                validate_persisted_project_path(cwd)?;
                Ok(Self::Project {
                    cwd: cwd
                        .to_str()
                        .ok_or(CodingAgentWorkspaceResolutionError::PersistedProjectPathNotUnicode)?
                        .to_owned(),
                })
            }
            CodingAgentWorkspaceScope::Projectless { workspace_id } => {
                validate_workspace_id(workspace_id)?;
                Ok(Self::Projectless {
                    workspace_id: workspace_id.clone(),
                })
            }
            CodingAgentWorkspaceScope::Legacy { .. } => {
                Err(CodingAgentWorkspaceResolutionError::LegacyCwdMissing)
            }
        }
    }

    pub(crate) fn to_product(
        &self,
    ) -> Result<CodingAgentWorkspaceScope, CodingAgentWorkspaceResolutionError> {
        match self {
            Self::Project { cwd } => {
                let cwd = PathBuf::from(cwd);
                validate_persisted_project_path(&cwd)?;
                Ok(CodingAgentWorkspaceScope::Project { cwd })
            }
            Self::Projectless { workspace_id } => {
                validate_workspace_id(workspace_id)?;
                Ok(CodingAgentWorkspaceScope::Projectless {
                    workspace_id: workspace_id.clone(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionManifest {
    pub schema: String,
    pub version: u32,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
    #[serde(default = "default_agent_profile_id")]
    pub default_agent_profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_scope: Option<PersistedWorkspaceScope>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub workspace_migrated_from_legacy: bool,
    pub event_log: String,
    #[serde(default = "default_outbox_log_file")]
    pub outbox_log: String,
}

impl SessionManifest {
    pub(crate) fn new(
        session_id: impl Into<String>,
        created_at: impl Into<String>,
        workspace_scope: PersistedWorkspaceScope,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            schema: SESSION_SCHEMA.into(),
            version: SESSION_VERSION,
            session_id: session_id.into(),
            name: None,
            updated_at: created_at.clone(),
            created_at,
            active_branch_id: None,
            active_leaf_id: None,
            default_agent_profile_id: default_agent_profile_id(),
            workspace_scope: Some(workspace_scope),
            workspace_migrated_from_legacy: false,
            event_log: SESSION_EVENT_LOG_FILE.into(),
            outbox_log: SESSION_OUTBOX_LOG_FILE.into(),
        }
    }

    pub(crate) fn with_default_agent_profile_id(mut self, profile_id: ProfileId) -> Self {
        self.default_agent_profile_id = profile_id;
        self
    }

    pub(crate) fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }
}

pub(crate) fn default_agent_profile_id() -> ProfileId {
    ProfileId::from("default")
}

fn default_outbox_log_file() -> String {
    SESSION_OUTBOX_LOG_FILE.into()
}

const fn is_false(value: &bool) -> bool {
    !*value
}
