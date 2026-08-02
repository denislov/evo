use crate::app::bootstrap::{SessionMode, SessionRunOptions};
use crate::app::error::ApplicationError;
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::authorization::ToolAuthorizationMode;
use crate::runtime::facade::{
    CodingAgentPublicError, CodingAgentSession, CodingAgentSessionHydration,
    CodingAgentSessionOpenTarget, CodingAgentSessionOptions, CodingAgentSessionOverview,
    CodingAgentSessionTranscriptItem, CodingAgentSessionTree, CodingAgentTranscriptContinuation,
    CodingSessionError, ProfileId,
};
use agent_core::api::transcript::{SessionEntry, SessionTreeNode};
use ai::api::client::AiClient;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::workspace::CodingAgentWorkspaceMigration;

const MAX_SESSION_QUERY_CHOICES: usize = 256;
const MAX_SESSION_QUERY_TRANSCRIPT_ITEMS: usize = 10_000;
const MAX_SESSION_QUERY_TREE_NODES: usize = 10_000;
const MAX_SESSION_TREE_PREVIEW_CHARS: usize = 200;

pub fn default_sessions_root() -> Result<PathBuf, ApplicationError> {
    let global_dir = match std::env::var_os("EVO_DIR") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".evo"),
    };
    Ok(global_dir.join("sessions"))
}

pub fn resolve_session_dir(
    _cwd: &Path,
    requested_session_dir: Option<&str>,
    runtime_session_dir: Option<&Path>,
) -> Result<PathBuf, ApplicationError> {
    if let Some(dir) = requested_session_dir {
        return Ok(PathBuf::from(dir));
    }

    if let Some(dir) = runtime_session_dir {
        return Ok(dir.to_path_buf());
    }

    if let Ok(env_dir) = std::env::var("EVO_SESSION_DIR") {
        return Ok(PathBuf::from(env_dir));
    }

    default_sessions_root()
}

#[derive(Debug, Clone)]
pub enum ResolvedSessionTarget {
    New,
    ContinueMostRecent,
    OpenTarget(String),
    OpenOrCreateId(String),
    ForkTarget(String),
}

/// Opaque product-owned bootstrap handle for one adapter session owner.
///
/// Session roots, explicit legacy/path targets, repository identity, and
/// authorization policy remain private. Public adapters may select durable
/// sessions only by product session ID.
#[derive(Clone)]
pub struct CodingAgentSessionBootstrap {
    session_options: Option<SessionRunOptions>,
    target: Option<ResolvedSessionTarget>,
    initial_session_name: Option<String>,
    default_agent_profile_id: ProfileId,
}

impl fmt::Debug for CodingAgentSessionBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentSessionBootstrap")
            .field(
                "persistent",
                &self
                    .session_options
                    .as_ref()
                    .is_some_and(|options| matches!(options.mode, SessionMode::Enabled)),
            )
            .field("target", &self.target_kind())
            .field("default_agent_profile_id", &self.default_agent_profile_id)
            .finish_non_exhaustive()
    }
}

impl CodingAgentSessionBootstrap {
    pub(crate) fn from_internal(
        session_options: Option<SessionRunOptions>,
        target: Option<ResolvedSessionTarget>,
        initial_session_name: Option<String>,
        default_agent_profile_id: ProfileId,
    ) -> Self {
        Self {
            session_options,
            target,
            initial_session_name,
            default_agent_profile_id,
        }
    }

    /// Select a persistent session by product identity.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.target = Some(ResolvedSessionTarget::OpenTarget(session_id.into()));
        self.initial_session_name = None;
        self
    }

    /// Select a new session without exposing its durable path or repository.
    pub fn with_new_session(mut self) -> Self {
        self.target = Some(ResolvedSessionTarget::New);
        self.initial_session_name = None;
        self
    }

    /// Allocate a fresh product session identity while retaining this
    /// handle's private persistence configuration.
    pub fn with_fresh_session(mut self) -> Self {
        self.target = Some(ResolvedSessionTarget::OpenOrCreateId(
            agent_core::api::transcript::create_session_id(),
        ));
        self.initial_session_name = None;
        self
    }

    /// Fork a durable session by product identity without exposing repository
    /// paths to the application adapter.
    pub fn with_forked_session(mut self, session_id: impl Into<String>) -> Self {
        self.target = Some(ResolvedSessionTarget::ForkTarget(session_id.into()));
        self.initial_session_name = None;
        self
    }

    /// Open an isolated in-memory session while retaining product-owned
    /// working-directory and profile defaults.
    pub fn without_persistence(mut self) -> Self {
        let cwd = self
            .session_options
            .as_ref()
            .map(|options| options.cwd.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        self.session_options = Some(SessionRunOptions::disabled(cwd));
        self.target = None;
        self.initial_session_name = None;
        self
    }

    /// Whether this handle opens durable product sessions.
    pub fn is_persistent(&self) -> bool {
        self.session_options
            .as_ref()
            .is_some_and(|options| matches!(options.mode, SessionMode::Enabled))
    }

    pub fn with_default_agent_profile_id(mut self, profile_id: ProfileId) -> Self {
        self.default_agent_profile_id = profile_id;
        self
    }

    pub fn without_initial_session_name(mut self) -> Self {
        self.initial_session_name = None;
        self
    }

    pub fn inherit_initial_session_name_from(mut self, current: &Self) -> Self {
        self.initial_session_name = current.initial_session_name.clone();
        self
    }

    pub(crate) fn initial_session_name(&self) -> Option<&str> {
        self.initial_session_name.as_deref()
    }

    pub(crate) fn hydrate_selected_internal(
        &self,
    ) -> Result<Option<CodingAgentSessionSnapshot>, ApplicationError> {
        hydrate_interactive_session_target(&self.session_options, self.target.as_ref())
    }

    /// Return the selected durable-session projection, if this bootstrap
    /// targets an existing session.
    pub fn selected_snapshot(
        &self,
    ) -> Result<Option<CodingAgentSessionSnapshot>, CodingAgentPublicError> {
        self.hydrate_selected_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub async fn open(&self) -> Result<CodingAgentSession, CodingAgentPublicError> {
        self.open_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_internal(&self) -> Result<CodingAgentSession, CodingSessionError> {
        let Some(session_options) = self.session_options.as_ref() else {
            return CodingAgentSession::non_persistent_internal(
                CodingAgentSessionOptions::new()
                    .with_default_agent_profile_id(self.default_agent_profile_id.clone())
                    .with_tool_authorization_mode(ToolAuthorizationMode::Interactive),
            )
            .await;
        };
        if !matches!(session_options.mode, SessionMode::Enabled) {
            return CodingAgentSession::non_persistent_internal(
                session_options_for_run(session_options)
                    .with_default_agent_profile_id(self.default_agent_profile_id.clone())
                    .with_tool_authorization_mode(ToolAuthorizationMode::Interactive),
            )
            .await;
        }

        let session_root = headless_session_root(session_options)?;
        let mut options = session_options_for_run(session_options)
            .with_session_log_root(session_root)
            .with_default_agent_profile_id(self.default_agent_profile_id.clone())
            .with_tool_authorization_mode(ToolAuthorizationMode::Interactive);
        if let Some(name) = self.initial_session_name.as_deref() {
            options = options.with_session_name(name);
        }
        match self.target.as_ref().unwrap_or(&ResolvedSessionTarget::New) {
            ResolvedSessionTarget::New => CodingAgentSession::create_internal(options).await,
            ResolvedSessionTarget::OpenOrCreateId(session_id) => {
                CodingAgentSession::open_or_create_internal(
                    options.with_session_id(session_id.clone()),
                )
                .await
            }
            ResolvedSessionTarget::OpenTarget(target) => {
                if target_looks_like_rust_native_session_dir(target) {
                    CodingAgentSession::open_internal(options.with_session_path(target)).await
                } else if target_looks_like_legacy_jsonl(target) {
                    Err(CodingSessionError::UnsupportedCapability {
                        capability: "legacy JSONL session targets".into(),
                    })
                } else {
                    CodingAgentSession::open_internal(options.with_session_id(target.clone())).await
                }
            }
            ResolvedSessionTarget::ContinueMostRecent => {
                let session_id = CodingAgentSession::list_internal(options.clone())?
                    .into_iter()
                    .next()
                    .map(|summary| summary.session_id)
                    .ok_or_else(|| CodingSessionError::Session {
                        message: "no previous session to continue".into(),
                    })?;
                CodingAgentSession::open_internal(options.with_session_id(session_id)).await
            }
            ResolvedSessionTarget::ForkTarget(source) => {
                let forked = CodingAgentSession::fork_session(
                    options.clone().with_session_id(source.clone()),
                    None,
                )?;
                CodingAgentSession::open_internal(
                    options.with_session_id(forked.summary.session_id),
                )
                .await
            }
        }
    }

    fn target_kind(&self) -> &'static str {
        match self.target.as_ref() {
            None | Some(ResolvedSessionTarget::New) => "new",
            Some(ResolvedSessionTarget::ContinueMostRecent) => "continue",
            Some(ResolvedSessionTarget::OpenTarget(_)) => "open",
            Some(ResolvedSessionTarget::OpenOrCreateId(_)) => "open_or_create",
            Some(ResolvedSessionTarget::ForkTarget(_)) => "fork",
        }
    }
}

/// One authority-free session choice returned by [`CodingAgentSessionQuery`].
///
/// The durable session directory and repository identity intentionally remain
/// private. Adapters use `session_id` for subsequent product queries and
/// commands; `working_directory` is display metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionChoice {
    pub id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub entry_count: usize,
    pub active_leaf_id: Option<String>,
    pub kind: CodingAgentSessionChoiceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentSessionChoiceKind {
    Persistent,
}

impl CodingAgentSessionChoice {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn searchable_text(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.id,
            self.name.as_deref().unwrap_or_default(),
            self.cwd,
            self.created_at,
            self.updated_at
        )
    }

    pub fn matches_target(&self, target: &str) -> bool {
        self.id == target || self.id.starts_with(target)
    }
}

/// Bounded result of discovering sessions for one product embedding context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionCatalog {
    pub choices: Vec<CodingAgentSessionChoice>,
    pub truncated: bool,
}

/// Bounded lightweight directory for idle and session-picker surfaces.
///
/// Each entry combines manifest facts with a product-owned workspace overview.
/// Legacy v1 identity reads only the first `SessionCreated` frame. Unlike
/// [`CodingAgentSessionCatalog`], this query does not replay transcripts and
/// therefore has no `entry_count`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionOverviewCatalog {
    pub overviews: Vec<CodingAgentSessionOverview>,
    pub truncated: bool,
}

/// Product-owned cumulative usage facts for a hydrated session.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodingAgentSessionUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub cost: f64,
    pub cost_known: bool,
    pub last_context_tokens: Option<u32>,
}

/// Bounded, read-only session projection used to resume adapter presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentSessionSnapshot {
    pub choice: CodingAgentSessionChoice,
    pub transcript: Vec<CodingAgentSessionTranscriptItem>,
    pub omitted_transcript_items: usize,
    pub continuation: Option<CodingAgentTranscriptContinuation>,
    pub usage: CodingAgentSessionUsage,
}

/// Adapter-neutral message role used by the session-tree presentation query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentSessionTreeRole {
    User,
    Assistant,
    ToolResult,
    Other,
}

/// One safe, presentation-ready node in a read-only session-tree projection.
///
/// Raw durable entries and arbitrary JSON fields remain private. The bounded
/// `display_text` is a product projection; adapters retain filtering, search,
/// folding, connector layout, and rendering policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionTreeNode {
    pub entry_id: String,
    pub entry_type: String,
    pub parent_id: Option<String>,
    pub children: Vec<CodingAgentSessionTreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
    pub display_text: String,
    pub message_role: Option<CodingAgentSessionTreeRole>,
    pub assistant_has_text: bool,
    pub assistant_stop_reason: Option<String>,
    pub assistant_error_message: Option<String>,
}

/// Complete read-only tree projection for one session, subject to a fixed
/// product-side node bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentSessionTreeSnapshot {
    pub roots: Vec<CodingAgentSessionTreeNode>,
    pub active_leaf_id: Option<String>,
}

/// Product-owned read/query port for durable session discovery and navigation.
///
/// The handle contains no mutable session owner. Its private options retain
/// repository-root and cwd authority so adapters never receive or reconstruct
/// durable session paths.
#[derive(Clone)]
pub struct CodingAgentSessionQuery {
    options: Option<CodingAgentSessionOptions>,
}

impl fmt::Debug for CodingAgentSessionQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentSessionQuery")
            .field("persistent", &self.options.is_some())
            .finish_non_exhaustive()
    }
}

impl CodingAgentSessionQuery {
    pub const fn disabled() -> Self {
        Self { options: None }
    }

    /// Build a bounded session navigation port for the product-global root.
    ///
    /// Resolution preserves the existing product precedence:
    /// `EVO_SESSION_DIR`, then `EVO_DIR/sessions`, then `~/.evo/sessions`.
    /// This does not load project configuration or create a session runtime.
    pub fn global() -> Result<Self, CodingAgentPublicError> {
        resolve_session_dir(Path::new("."), None, None)
            .map(Self::from_session_root)
            .map_err(CodingAgentPublicError::from)
    }

    /// Build a bounded session navigation port for an explicit durable root.
    ///
    /// The remaining repository inputs use product defaults so callers do not
    /// need a cwd, profile registry, or [`CodingAgentEmbeddingContext`](crate::api::embedding::CodingAgentEmbeddingContext).
    pub fn from_session_root(root: impl Into<PathBuf>) -> Self {
        Self {
            options: Some(
                CodingAgentSessionOptions::new()
                    .with_session_log_root(root)
                    .with_default_agent_profile_id(ProfileId::from("default")),
            ),
        }
    }

    pub(crate) fn from_run_options(
        session_options: &Option<SessionRunOptions>,
    ) -> Result<Self, CodingSessionError> {
        let options = enabled_session_options(session_options)
            .map(interactive_navigation_options)
            .transpose()?;
        Ok(Self { options })
    }

    pub(crate) fn from_run_options_unscoped(
        session_options: &Option<SessionRunOptions>,
    ) -> Result<Self, CodingSessionError> {
        let options = enabled_session_options(session_options)
            .map(interactive_navigation_options)
            .transpose()?
            .map(CodingAgentSessionOptions::without_workspace_filter);
        Ok(Self { options })
    }

    pub fn catalog(&self) -> Result<CodingAgentSessionCatalog, CodingAgentPublicError> {
        self.catalog_internal()
            .map_err(CodingAgentPublicError::from)
    }

    /// List durable sessions without replaying their event logs.
    ///
    /// Current manifests provide workspace identity directly. For legacy v1
    /// candidates only the first bounded, checksummed `SessionCreated` frame
    /// is decoded for migration. Later event frames, transcript state, and
    /// usage are never read.
    pub fn overviews(&self) -> Result<CodingAgentSessionOverviewCatalog, CodingAgentPublicError> {
        self.overviews_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn overviews_internal(
        &self,
    ) -> Result<CodingAgentSessionOverviewCatalog, CodingSessionError> {
        let Some(options) = &self.options else {
            return Ok(CodingAgentSessionOverviewCatalog {
                overviews: Vec::new(),
                truncated: false,
            });
        };
        let (overviews, truncated) = CodingAgentSession::list_overviews_internal(
            options.clone(),
            MAX_SESSION_QUERY_CHOICES,
        )?;
        Ok(CodingAgentSessionOverviewCatalog {
            overviews,
            truncated,
        })
    }

    /// Explicitly migrate one legacy session's workspace identity.
    ///
    /// A valid legacy Project or Projectless identity is atomically written to
    /// the current manifest schema. Missing or invalid legacy identity remains
    /// readable and returns an `Unavailable` outcome without writing Legacy.
    pub fn migrate_workspace(
        &self,
        session_id: impl AsRef<str>,
    ) -> Result<CodingAgentWorkspaceMigration, CodingAgentPublicError> {
        let options = self
            .options_for_session(session_id.as_ref())
            .map_err(CodingAgentPublicError::from)?;
        crate::session::service::SessionService::migrate_workspace(&options)
            .map_err(CodingAgentPublicError::from)
    }

    /// Resolve and, when possible, migrate the typed workspace needed to open
    /// one durable session. This reads no transcript state and exposes no
    /// session repository path.
    pub fn open_target(
        &self,
        session_id: impl AsRef<str>,
    ) -> Result<CodingAgentSessionOpenTarget, CodingAgentPublicError> {
        let options = self
            .options_for_session(session_id.as_ref())
            .map_err(CodingAgentPublicError::from)?;
        crate::session::service::SessionService::open_target(&options)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn catalog_internal(&self) -> Result<CodingAgentSessionCatalog, CodingSessionError> {
        let Some(options) = &self.options else {
            return Ok(CodingAgentSessionCatalog {
                choices: Vec::new(),
                truncated: false,
            });
        };
        let summaries = CodingAgentSession::list_internal(options.clone())?;
        let truncated = summaries.len() > MAX_SESSION_QUERY_CHOICES;
        let choices = summaries
            .into_iter()
            .take(MAX_SESSION_QUERY_CHOICES)
            .filter_map(|summary| {
                CodingAgentSession::hydrate(
                    options.clone().with_session_id(summary.session_id.clone()),
                )
                .ok()
            })
            .map(session_snapshot_from_hydration)
            .map(|snapshot| snapshot.choice)
            .collect();
        Ok(CodingAgentSessionCatalog { choices, truncated })
    }

    pub fn snapshot(
        &self,
        session_id: impl AsRef<str>,
    ) -> Result<CodingAgentSessionSnapshot, CodingAgentPublicError> {
        self.snapshot_internal(session_id.as_ref())
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn snapshot_internal(
        &self,
        session_id: &str,
    ) -> Result<CodingAgentSessionSnapshot, CodingSessionError> {
        let options = self.options_for_session(session_id)?;
        CodingAgentSession::hydrate(options).map(session_snapshot_from_hydration)
    }

    pub fn clone_session(
        &self,
        session_id: impl AsRef<str>,
    ) -> Result<CodingAgentSessionSnapshot, CodingAgentPublicError> {
        self.clone_session_internal(session_id.as_ref())
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn clone_session_internal(
        &self,
        session_id: &str,
    ) -> Result<CodingAgentSessionSnapshot, CodingSessionError> {
        CodingAgentSession::clone_session(self.options_for_session(session_id)?)
            .map(session_snapshot_from_hydration)
    }

    pub fn tree(
        &self,
        session_id: impl AsRef<str>,
    ) -> Result<CodingAgentSessionTreeSnapshot, CodingAgentPublicError> {
        self.tree_internal(session_id.as_ref())
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn tree_internal(
        &self,
        session_id: &str,
    ) -> Result<CodingAgentSessionTreeSnapshot, CodingSessionError> {
        let tree = CodingAgentSession::tree_view(self.options_for_session(session_id)?)?;
        project_session_tree(tree)
    }

    pub fn export_html(
        &self,
        session_id: impl AsRef<str>,
        output_path: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingAgentPublicError> {
        self.export_html_internal(session_id.as_ref(), output_path.as_ref())
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn export_html_internal(
        &self,
        session_id: &str,
        output_path: &Path,
    ) -> Result<PathBuf, CodingSessionError> {
        CodingAgentSession::export_session_html_internal(
            self.options_for_session(session_id)?,
            output_path,
        )
    }

    fn options_for_session(
        &self,
        session_id: &str,
    ) -> Result<CodingAgentSessionOptions, CodingSessionError> {
        self.options
            .clone()
            .map(|options| options.with_session_id(session_id.to_owned()))
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: "durable session queries while persistence is disabled".into(),
            })
    }
}

impl CodingAgentSession {
    /// Returns the bounded presentation snapshot for the current persistent
    /// session. Non-persistent sessions have no durable navigation snapshot.
    pub fn current_session_snapshot(
        &self,
    ) -> Result<Option<CodingAgentSessionSnapshot>, CodingAgentPublicError> {
        self.hydrate_current()
            .map(|hydration| hydration.map(session_snapshot_from_hydration))
            .map_err(CodingAgentPublicError::from)
    }
}

pub(crate) fn session_snapshot_from_hydration(
    hydration: CodingAgentSessionHydration,
) -> CodingAgentSessionSnapshot {
    let retained_entry_count = hydration.transcript.len();
    let locally_omitted = retained_entry_count.saturating_sub(MAX_SESSION_QUERY_TRANSCRIPT_ITEMS);
    let entry_count = retained_entry_count.saturating_add(hydration.omitted_items);
    let omitted_transcript_items = hydration.omitted_items.saturating_add(locally_omitted);
    let transcript = hydration
        .transcript
        .into_iter()
        .skip(locally_omitted)
        .collect();
    CodingAgentSessionSnapshot {
        choice: CodingAgentSessionChoice {
            id: hydration.summary.session_id,
            name: hydration.summary.name,
            cwd: hydration.cwd.unwrap_or_default(),
            created_at: hydration.summary.created_at,
            updated_at: hydration.summary.updated_at,
            entry_count,
            active_leaf_id: hydration.summary.active_leaf_id,
            kind: CodingAgentSessionChoiceKind::Persistent,
        },
        transcript,
        omitted_transcript_items,
        continuation: hydration.continuation,
        usage: CodingAgentSessionUsage {
            input: hydration.usage.input,
            output: hydration.usage.output,
            cache_read: hydration.usage.cache_read,
            cache_write: hydration.usage.cache_write,
            cost: hydration.usage.cost,
            cost_known: hydration.usage.cost_known,
            last_context_tokens: hydration.usage.last_context_tokens,
        },
    }
}

fn project_session_tree(
    tree: CodingAgentSessionTree,
) -> Result<CodingAgentSessionTreeSnapshot, CodingSessionError> {
    let node_count = tree
        .tree
        .iter()
        .map(count_session_tree_nodes)
        .sum::<usize>();
    if node_count > MAX_SESSION_QUERY_TREE_NODES {
        return Err(CodingSessionError::Resource {
            message: format!(
                "session tree contains {node_count} nodes; query limit is {MAX_SESSION_QUERY_TREE_NODES}"
            ),
        });
    }
    let tool_calls = collect_tree_tool_calls(&tree.tree);
    Ok(CodingAgentSessionTreeSnapshot {
        roots: tree
            .tree
            .iter()
            .map(|node| project_session_tree_node(node, &tool_calls))
            .collect(),
        active_leaf_id: tree.active_leaf_id,
    })
}

fn count_session_tree_nodes(node: &SessionTreeNode) -> usize {
    1 + node
        .children
        .iter()
        .map(count_session_tree_nodes)
        .sum::<usize>()
}

#[derive(Debug)]
struct SessionTreeToolCall {
    name: String,
    arguments: serde_json::Value,
}

fn project_session_tree_node(
    node: &SessionTreeNode,
    tool_calls: &BTreeMap<String, SessionTreeToolCall>,
) -> CodingAgentSessionTreeNode {
    let message_role = session_tree_message_role(&node.entry).map(|role| match role {
        "user" => CodingAgentSessionTreeRole::User,
        "assistant" => CodingAgentSessionTreeRole::Assistant,
        "toolResult" => CodingAgentSessionTreeRole::ToolResult,
        _ => CodingAgentSessionTreeRole::Other,
    });
    CodingAgentSessionTreeNode {
        entry_id: node.entry.id.clone(),
        entry_type: node.entry.entry_type.clone(),
        parent_id: node.entry.parent_id.clone(),
        children: node
            .children
            .iter()
            .map(|child| project_session_tree_node(child, tool_calls))
            .collect(),
        label: node
            .label
            .as_deref()
            .map(|label| normalized_preview(label, MAX_SESSION_TREE_PREVIEW_CHARS)),
        label_timestamp: node.label_timestamp.clone(),
        display_text: session_tree_entry_display_text(node, tool_calls),
        message_role,
        assistant_has_text: session_tree_assistant_has_text(&node.entry),
        assistant_stop_reason: session_tree_assistant_field(&node.entry, "stopReason"),
        assistant_error_message: session_tree_assistant_field(&node.entry, "errorMessage")
            .map(|message| normalized_preview(&message, MAX_SESSION_TREE_PREVIEW_CHARS)),
    }
}

fn session_tree_message_role(entry: &SessionEntry) -> Option<&str> {
    if entry.entry_type != "message" {
        return None;
    }
    entry
        .field("message")
        .and_then(|message| message.get("role"))
        .and_then(|role| role.as_str())
}

fn collect_tree_tool_calls(tree: &[SessionTreeNode]) -> BTreeMap<String, SessionTreeToolCall> {
    fn walk(node: &SessionTreeNode, result: &mut BTreeMap<String, SessionTreeToolCall>) {
        if session_tree_message_role(&node.entry) == Some("assistant")
            && let Some(content) = node
                .entry
                .field("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_array())
        {
            for block in content {
                if block.get("type").and_then(|value| value.as_str()) == Some("toolCall")
                    && let (Some(id), Some(name)) = (
                        block.get("id").and_then(|value| value.as_str()),
                        block.get("name").and_then(|value| value.as_str()),
                    )
                {
                    result.insert(
                        id.to_owned(),
                        SessionTreeToolCall {
                            name: name.to_owned(),
                            arguments: block
                                .get("arguments")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        },
                    );
                }
            }
        }
        for child in &node.children {
            walk(child, result);
        }
    }

    let mut result = BTreeMap::new();
    for node in tree {
        walk(node, &mut result);
    }
    result
}

fn session_tree_assistant_has_text(entry: &SessionEntry) -> bool {
    if session_tree_message_role(entry) != Some("assistant") {
        return false;
    }
    entry
        .field("message")
        .and_then(|message| message.get("content"))
        .is_some_and(session_tree_content_has_text)
}

fn session_tree_assistant_field(entry: &SessionEntry, field: &str) -> Option<String> {
    if session_tree_message_role(entry) != Some("assistant") {
        return None;
    }
    entry
        .field("message")
        .and_then(|message| message.get(field))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn session_tree_content_has_text(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(blocks) => blocks.iter().any(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("text")
                && block
                    .get("text")
                    .and_then(|value| value.as_str())
                    .is_some_and(|text| !text.trim().is_empty())
        }),
        _ => false,
    }
}

mod display;

use display::{normalized_preview, session_tree_entry_display_text};

mod hydration;

use hydration::{
    enabled_session_options, headless_session_root, interactive_navigation_options,
    session_options_for_run, target_looks_like_legacy_jsonl,
    target_looks_like_rust_native_session_dir,
};
pub(crate) use hydration::{
    hydrate_interactive_session_target, open_headless_prompt_session, runtime_session_root,
};
