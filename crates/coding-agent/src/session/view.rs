use serde::Serialize;
use std::path::{Path, PathBuf};

use agent_core::api::transcript::SessionTreeNode;
use ai::api::client::AiClient;

use crate::authorization::ToolAuthorizationMode;
use crate::profiles::{ProfileId, ProfileKind};
use crate::workspace::{
    CodingAgentResolvedWorkspace, CodingAgentWorkspaceMigration, CodingAgentWorkspaceOverview,
    CodingAgentWorkspaceScope,
};

#[derive(Clone, Default)]
pub struct CodingAgentSessionOptions {
    cwd: Option<PathBuf>,
    workspace_scope: Option<CodingAgentWorkspaceScope>,
    workspace_global_config_dir: Option<PathBuf>,
    session_id: Option<String>,
    session_name: Option<String>,
    session_log_root: Option<PathBuf>,
    session_path: Option<PathBuf>,
    default_agent_profile_id: Option<ProfileId>,
    ai_client: Option<AiClient>,
    tool_authorization_mode: ToolAuthorizationMode,
    worktree_registry_dir: Option<PathBuf>,
    extension_host_options: Option<extension_host::api::ExtensionHostOptions>,
}

impl std::fmt::Debug for CodingAgentSessionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodingAgentSessionOptions")
            .field("cwd", &self.cwd)
            .field("workspace_scope", &self.workspace_scope)
            .field("session_id", &self.session_id)
            .field("has_session_name", &self.session_name.is_some())
            .field("session_log_root", &self.session_log_root)
            .field("session_path", &self.session_path)
            .field("default_agent_profile_id", &self.default_agent_profile_id)
            .field("has_scoped_ai_client", &self.ai_client.is_some())
            .field("tool_authorization_mode", &self.tool_authorization_mode)
            .field("has_extension_host", &self.extension_host_options.is_some())
            .finish()
    }
}

impl CodingAgentSessionOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_resolved_workspace(mut self, workspace: CodingAgentResolvedWorkspace) -> Self {
        self.cwd = Some(workspace.execution_cwd);
        self.workspace_scope = Some(workspace.scope);
        self
    }

    /// Keep repository authority while removing workspace-specific list filters.
    pub(crate) fn without_workspace_filter(mut self) -> Self {
        self.cwd = None;
        self.workspace_scope = None;
        self
    }

    pub fn with_session_log_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.session_log_root = Some(root.into());
        self
    }

    pub fn with_session_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.session_path = Some(path.into());
        self
    }

    pub fn with_default_agent_profile_id(mut self, profile_id: impl Into<ProfileId>) -> Self {
        self.default_agent_profile_id = Some(profile_id.into());
        self
    }

    pub fn with_ai_client(mut self, ai_client: AiClient) -> Self {
        self.ai_client = Some(ai_client);
        self
    }

    pub fn with_tool_authorization_mode(mut self, mode: ToolAuthorizationMode) -> Self {
        self.tool_authorization_mode = mode;
        self
    }

    /// Pin the managed-worktree registry root.
    ///
    /// Defaults to the user-global config directory's `worktrees` directory.
    /// Tests override this to keep child worktrees out of real user state.
    pub fn with_worktree_registry_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.worktree_registry_dir = Some(dir.into());
        self
    }

    /// 启用真实 extension host（user hooks）。
    ///
    /// 默认（`None`）保持「无 host」行为（Noop 端口，与 ARC-700 一致）；
    /// 显式传入 [`ExtensionHostOptions`] 时装配真实 host：项目/全局扩展
    /// 目录被发现、trust 判定、事件派发与 Tool/Stop gate 全部生效。
    pub fn with_extension_host_options(
        mut self,
        options: extension_host::api::ExtensionHostOptions,
    ) -> Self {
        self.extension_host_options = Some(options);
        self
    }

    pub(crate) fn extension_host_options(
        &self,
    ) -> Option<&extension_host::api::ExtensionHostOptions> {
        self.extension_host_options.as_ref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub(crate) fn workspace_scope(&self) -> Option<&CodingAgentWorkspaceScope> {
        self.workspace_scope.as_ref()
    }

    pub(crate) fn workspace_global_config_dir(&self) -> Option<&Path> {
        self.workspace_global_config_dir.as_deref()
    }

    pub fn session_log_root(&self) -> Option<&Path> {
        self.session_log_root.as_deref()
    }

    pub fn session_path(&self) -> Option<&Path> {
        self.session_path.as_deref()
    }

    pub fn default_agent_profile_id(&self) -> Option<&ProfileId> {
        self.default_agent_profile_id.as_ref()
    }

    pub(crate) fn ai_client(&self) -> Option<&AiClient> {
        self.ai_client.as_ref()
    }

    pub(crate) fn tool_authorization_mode(&self) -> ToolAuthorizationMode {
        self.tool_authorization_mode
    }

    pub(crate) fn worktree_registry_dir(&self) -> Option<&Path> {
        self.worktree_registry_dir.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentSessionView {
    pub session_id: String,
    pub name: Option<String>,
    pub default_agent_profile_id: ProfileId,
}

impl CodingAgentSessionView {
    pub fn new(
        session_id: impl Into<String>,
        name: Option<String>,
        default_agent_profile_id: ProfileId,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            name,
            default_agent_profile_id,
        }
    }
}

/// A committed durable-session name change observed after subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionNameUpdate {
    pub name: Option<String>,
    pub updated_at: String,
}

/// Receiver for committed session-name changes, including automatic naming.
#[derive(Debug)]
pub struct CodingAgentSessionNameUpdateReceiver {
    pub(crate) inner: tokio::sync::watch::Receiver<crate::session::service::SessionNameUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CodingAgentRecoveryPending {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    #[serde(rename = "recoveryId")]
    pub recovery_id: String,
    pub operation_kind: Option<String>,
    #[serde(rename = "recordVersion")]
    pub record_version: u64,
    #[serde(rename = "descriptorRevision")]
    pub descriptor_revision: u16,
    #[serde(rename = "capabilityGeneration")]
    pub capability_generation: Option<u64>,
    #[serde(rename = "attemptCount")]
    pub attempt_count: u32,
    #[serde(rename = "lastAttemptAt")]
    pub last_attempt_at: Option<String>,
    #[serde(rename = "nextAttemptAt")]
    pub next_attempt_at: Option<String>,
}

impl CodingAgentRecoveryPending {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        operation_id: impl Into<String>,
        recovery_id: impl Into<String>,
        operation_kind: Option<String>,
        record_version: u64,
        descriptor_revision: u16,
        capability_generation: Option<u64>,
        attempt_count: u32,
        last_attempt_at: Option<String>,
        next_attempt_at: Option<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            recovery_id: recovery_id.into(),
            operation_kind,
            record_version,
            descriptor_revision,
            capability_generation,
            attempt_count,
            last_attempt_at,
            next_attempt_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentRecoveryResolutionRequest {
    pub operation_id: String,
    pub recovery_id: String,
    pub expected_record_version: u64,
    pub expected_descriptor_revision: u16,
    pub expected_capability_generation: Option<u64>,
    pub expected_attempt_count: u32,
    pub resolution: crate::events::CodingAgentRecoveryResolution,
    pub reason: String,
}

impl CodingAgentRecoveryResolutionRequest {
    pub fn from_pending(
        pending: &CodingAgentRecoveryPending,
        resolution: crate::events::CodingAgentRecoveryResolution,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: pending.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            expected_record_version: pending.record_version,
            expected_descriptor_revision: pending.descriptor_revision,
            expected_capability_generation: pending.capability_generation,
            expected_attempt_count: pending.attempt_count,
            resolution,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentRecoveryResolutionResult {
    pub operation_id: String,
    pub recovery_id: String,
    pub resolution: crate::events::CodingAgentRecoveryResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentRecoveryRetryRequest {
    pub operation_id: String,
    pub recovery_id: String,
    pub expected_record_version: u64,
    pub expected_descriptor_revision: u16,
    pub expected_capability_generation: Option<u64>,
    pub expected_attempt_count: u32,
    pub schedule_with_backoff: bool,
}

impl CodingAgentRecoveryRetryRequest {
    pub fn from_pending(pending: &CodingAgentRecoveryPending) -> Self {
        Self {
            operation_id: pending.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            expected_record_version: pending.record_version,
            expected_descriptor_revision: pending.descriptor_revision,
            expected_capability_generation: pending.capability_generation,
            expected_attempt_count: pending.attempt_count,
            schedule_with_backoff: false,
        }
    }

    pub fn with_backoff(mut self) -> Self {
        self.schedule_with_backoff = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentRecoveryRetryResult {
    pub operation_id: String,
    pub recovery_id: String,
    pub attempt_count: u32,
    pub last_attempt_at: String,
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentSessionSummary {
    pub session_id: String,
    pub name: Option<String>,
    pub storage: SessionStorageHandle,
    pub created_at: String,
    pub updated_at: String,
    pub active_leaf_id: Option<String>,
}

/// Read-only authority for one durable session's storage artifacts.
///
/// Repository directories remain private. Adapters may request an explicit
/// export destination or open the event log for legacy streaming protocols,
/// but cannot use this value as an arbitrary filesystem path.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionStorageHandle {
    session_id: String,
    directory: PathBuf,
    event_log_name: String,
}

impl SessionStorageHandle {
    pub(crate) fn new(session_id: String, directory: PathBuf, event_log_name: String) -> Self {
        Self {
            session_id,
            directory,
            event_log_name,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Explicit path intended for user-visible export/status compatibility.
    pub fn export_path(&self) -> PathBuf {
        self.directory.clone()
    }

    /// Open the authoritative append-only event log without exposing its path.
    pub fn open_event_log(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::open(self.directory.join(&self.event_log_name))
    }
}

impl std::fmt::Debug for SessionStorageHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionStorageHandle")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// Lightweight durable-session facts for list surfaces.
///
/// Current manifests carry workspace identity directly. Only legacy v1
/// manifests read the first `SessionCreated` frame for migration; no
/// transcript, usage, or later event is replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentSessionOverview {
    pub session_id: String,
    pub name: Option<String>,
    pub workspace: CodingAgentWorkspaceOverview,
    pub workspace_migration: CodingAgentWorkspaceMigration,
    pub created_at: String,
    pub updated_at: String,
    pub active_leaf_id: Option<String>,
}

/// Product-owned workspace identity required to open one durable session.
///
/// Unlike the authority-free list overview, this target retains the complete
/// typed scope needed to build an isolated embedding context. Legacy identity
/// is migrated before this value is returned whenever recovery is possible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentSessionOpenTarget {
    pub session_id: String,
    pub workspace_scope: CodingAgentWorkspaceScope,
    pub workspace_migration: CodingAgentWorkspaceMigration,
}

/// Complete read-only transcript projection for the current session leaf.
///
/// This DTO contains presentation facts only. It does not expose repository
/// paths, replay services, writer handles, or navigation authority.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CodingAgentTranscriptSnapshot {
    pub session_id: String,
    pub active_leaf_id: Option<String>,
    pub items: Vec<CodingAgentSessionTranscriptItem>,
    pub omitted_items: usize,
    pub continuation: Option<CodingAgentTranscriptContinuation>,
}

impl CodingAgentTranscriptSnapshot {
    pub fn new(
        session_id: impl Into<String>,
        active_leaf_id: Option<String>,
        items: Vec<CodingAgentSessionTranscriptItem>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            active_leaf_id,
            items,
            omitted_items: 0,
            continuation: None,
        }
    }

    pub(crate) fn new_bounded(
        session_id: impl Into<String>,
        active_leaf_id: Option<String>,
        items: Vec<CodingAgentSessionTranscriptItem>,
        omitted_items: usize,
        continuation: Option<CodingAgentTranscriptContinuation>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            active_leaf_id,
            items,
            omitted_items,
            continuation,
        }
    }
}

/// Opaque cursor for requesting durable transcript facts older than a bounded
/// hydration page without exposing repository paths or file offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodingAgentTranscriptContinuation {
    token: String,
    before_session_sequence: u64,
}

impl CodingAgentTranscriptContinuation {
    pub(crate) fn new(token: String, before_session_sequence: u64) -> Self {
        Self {
            token,
            before_session_sequence,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub const fn before_session_sequence(&self) -> u64 {
        self.before_session_sequence
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodingAgentSessionHydration {
    pub(crate) summary: CodingAgentSessionSummary,
    pub(crate) cwd: Option<String>,
    pub(crate) transcript: Vec<CodingAgentSessionTranscriptItem>,
    pub(crate) omitted_items: usize,
    pub(crate) continuation: Option<CodingAgentTranscriptContinuation>,
    pub(crate) diagnostics: Vec<CodingAgentSessionDiagnostic>,
    pub(crate) usage: CodingAgentSessionUsageSummary,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodingAgentSessionTree {
    pub(crate) tree: Vec<SessionTreeNode>,
    pub(crate) active_leaf_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodingAgentSessionUsageSummary {
    pub(crate) input: u32,
    pub(crate) output: u32,
    pub(crate) cache_read: u32,
    pub(crate) cache_write: u32,
    pub(crate) cost: f64,
    pub(crate) cost_known: bool,
    pub(crate) last_context_tokens: Option<u32>,
}

impl Default for CodingAgentSessionUsageSummary {
    fn default() -> Self {
        Self {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: 0.0,
            cost_known: true,
            last_context_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentSessionTranscriptItem {
    User {
        text: String,
        /// Wall-clock time the turn was submitted (RFC 3339); `None` for
        /// in-memory transcripts.
        started_at: Option<String>,
    },
    Assistant {
        id: String,
        text: String,
        thinking: String,
        images: Vec<crate::events::CodingAgentImageContent>,
        done: bool,
        /// Sum of completed reasoning-segment lifetimes. `None` means the
        /// lifecycle was not observed or the message is still streaming.
        reasoning_duration_millis: Option<u64>,
        /// Model that actually produced this message (`response_model` when
        /// the provider reported one, otherwise the requested model).
        model_id: Option<String>,
        /// Wall-clock completion time (RFC 3339) when the message finished
        /// streaming; `None` while running or for legacy session files.
        completed_at: Option<String>,
    },
    Tool {
        call_id: String,
        name: String,
        args: serde_json::Value,
        result: Option<String>,
        is_error: bool,
        /// Durable wall-clock duration between `tool.call.started` and its
        /// terminal session event. `None` means running or unavailable.
        duration_millis: Option<u64>,
    },
    Delegation {
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: String,
        child_operation_id: Option<String>,
        summary: Option<String>,
    },
    CompactionSummary {
        summary: String,
    },
    BranchSummary {
        summary: String,
    },
    Diagnostic {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodingAgentSessionDiagnostic {
    pub(crate) message: String,
}
