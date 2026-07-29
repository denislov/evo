use crate::app::bootstrap::{SessionMode, SessionRunOptions};
use crate::app::error::ApplicationError;
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::authorization::ToolAuthorizationMode;
use crate::runtime::facade::{
    CodingAgentPublicError, CodingAgentSession, CodingAgentSessionHydration,
    CodingAgentSessionOpenTarget, CodingAgentSessionOptions, CodingAgentSessionOverview,
    CodingAgentSessionTranscriptItem, CodingAgentSessionTree, CodingSessionError, ProfileId,
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
    let entry_count = hydration.transcript.len();
    let omitted_transcript_items = entry_count.saturating_sub(MAX_SESSION_QUERY_TRANSCRIPT_ITEMS);
    let transcript = hydration
        .transcript
        .into_iter()
        .skip(omitted_transcript_items)
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

fn session_tree_entry_display_text(
    node: &SessionTreeNode,
    tool_calls: &BTreeMap<String, SessionTreeToolCall>,
) -> String {
    let entry = &node.entry;
    match entry.entry_type.as_str() {
        "message" => session_tree_message_display_text(entry, tool_calls),
        "bashExecution" => {
            let command = entry
                .field("command")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let output = entry
                .field("output")
                .and_then(|value| value.as_str())
                .and_then(|output| output.lines().next())
                .map(|output| normalized_preview(output, 40))
                .unwrap_or_default();
            normalized_preview(
                &format!("[bash] {command} {output}"),
                MAX_SESSION_TREE_PREVIEW_CHARS,
            )
        }
        "toolResult" => {
            let name = entry
                .field("toolName")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let preview = entry
                .field("content")
                .and_then(|value| value.as_array())
                .and_then(|content| content.first())
                .and_then(|block| block.get("text"))
                .and_then(|value| value.as_str())
                .map(|text| normalized_preview(text, 40))
                .unwrap_or_default();
            format!("[toolResult] {name}: {preview}")
        }
        "compaction" => {
            let summary = entry
                .field("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("compacted");
            let tokens = entry
                .field("tokensBefore")
                .and_then(serde_json::Value::as_u64)
                .map(|tokens| (tokens as f64 / 1000.0).round() as u64)
                .unwrap_or(0);
            if tokens > 0 {
                format!("[compaction: {tokens}k tokens]")
            } else {
                format!(
                    "[compaction] {}",
                    normalized_preview(summary, MAX_SESSION_TREE_PREVIEW_CHARS)
                )
            }
        }
        "branch_summary" => {
            let summary = entry
                .field("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("branch");
            format!(
                "[branch summary]: {}",
                normalized_preview(summary, MAX_SESSION_TREE_PREVIEW_CHARS)
            )
        }
        "custom_message" | "custom" => {
            let custom_type = entry
                .field("customType")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[custom: {custom_type}]")
        }
        "session_info" => {
            let name = entry
                .field("name")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!(
                "[title: {}]",
                normalized_preview(name, MAX_SESSION_TREE_PREVIEW_CHARS)
            )
        }
        "model_change" => {
            let model = entry
                .field("modelId")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[model: {model}]")
        }
        "thinking_level_change" => {
            let level = entry
                .field("thinkingLevel")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("[thinking: {level}]")
        }
        _ => normalized_preview(
            &format!("[{}] {}", entry.entry_type, entry.id),
            MAX_SESSION_TREE_PREVIEW_CHARS,
        ),
    }
}

fn session_tree_message_display_text(
    entry: &SessionEntry,
    tool_calls: &BTreeMap<String, SessionTreeToolCall>,
) -> String {
    let Some(message) = entry.field("message") else {
        return entry.id.clone();
    };
    let Some(role) = message.get("role").and_then(|value| value.as_str()) else {
        return entry.id.clone();
    };
    let preview = session_tree_message_text_preview(message);
    match role {
        "user" => format!("user: {preview}"),
        "assistant" if !preview.is_empty() => format!("assistant: {preview}"),
        "assistant"
            if message.get("stopReason").and_then(|value| value.as_str()) == Some("aborted") =>
        {
            "assistant: (aborted)".to_owned()
        }
        "assistant" => message
            .get("errorMessage")
            .and_then(|value| value.as_str())
            .map(|error| {
                format!(
                    "assistant: {}",
                    normalized_preview(error, MAX_SESSION_TREE_PREVIEW_CHARS)
                )
            })
            .unwrap_or_else(|| "assistant: (no content)".to_owned()),
        "toolResult" => {
            let tool_call = message
                .get("toolCallId")
                .and_then(|value| value.as_str())
                .and_then(|id| tool_calls.get(id));
            if let Some(tool_call) = tool_call {
                format_session_tree_tool_call(&tool_call.name, &tool_call.arguments)
            } else {
                let name = message
                    .get("toolName")
                    .and_then(|value| value.as_str())
                    .unwrap_or("tool");
                format!("[{name}]")
            }
        }
        _ => normalized_preview(
            &format!("[{role}] {preview}"),
            MAX_SESSION_TREE_PREVIEW_CHARS,
        ),
    }
}

fn session_tree_message_text_preview(message: &serde_json::Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        serde_json::Value::String(text) => normalized_preview(text, MAX_SESSION_TREE_PREVIEW_CHARS),
        serde_json::Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if block.get("type").and_then(|value| value.as_str()) == Some("text")
                    && let Some(part) = block.get("text").and_then(|value| value.as_str())
                {
                    text.push_str(part);
                    if text.chars().count() >= MAX_SESSION_TREE_PREVIEW_CHARS {
                        break;
                    }
                }
            }
            normalized_preview(&text, MAX_SESSION_TREE_PREVIEW_CHARS)
        }
        _ => String::new(),
    }
}

fn normalized_preview(text: &str, max_chars: usize) -> String {
    let normalized = text.replace(['\n', '\t'], " ").trim().to_owned();
    if normalized.chars().count() > max_chars {
        normalized.chars().take(max_chars).collect()
    } else {
        normalized
    }
}

fn format_session_tree_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    let argument = |key: &str| arguments.get(key).and_then(|value| value.as_str());
    match name {
        "read" => {
            let path = shorten_session_tree_home(
                argument("path")
                    .or_else(|| argument("file_path"))
                    .unwrap_or(""),
            );
            let mut display = path;
            let offset = arguments.get("offset").and_then(serde_json::Value::as_i64);
            let limit = arguments.get("limit").and_then(serde_json::Value::as_i64);
            if offset.is_some() || limit.is_some() {
                let start = offset.unwrap_or(1);
                display.push(':');
                display.push_str(&start.to_string());
                if let Some(end) = limit.map(|limit| start + limit - 1) {
                    display.push('-');
                    display.push_str(&end.to_string());
                }
            }
            normalized_preview(
                &format!("[read: {display}]"),
                MAX_SESSION_TREE_PREVIEW_CHARS,
            )
        }
        "write" | "edit" => {
            let path = shorten_session_tree_home(
                argument("path")
                    .or_else(|| argument("file_path"))
                    .unwrap_or(""),
            );
            format!("[{name}: {path}]")
        }
        "bash" => {
            let raw = argument("command").unwrap_or("");
            let command = normalized_preview(raw, 50);
            let suffix = if raw.chars().count() > 50 { "..." } else { "" };
            format!("[bash: {command}{suffix}]")
        }
        "grep" | "find" => {
            let pattern = argument("pattern").unwrap_or("");
            let path = shorten_session_tree_home(argument("path").unwrap_or("."));
            let separator = if name == "grep" { "/" } else { "" };
            format!("[{name}: {separator}{pattern}{separator} in {path}]")
        }
        "ls" => {
            let path = shorten_session_tree_home(argument("path").unwrap_or("."));
            format!("[ls: {path}]")
        }
        _ => {
            let arguments = arguments.to_string();
            let preview = normalized_preview(&arguments, 40);
            let suffix = if arguments.chars().count() > 40 {
                "..."
            } else {
                ""
            };
            format!("[{name}: {preview}{suffix}]")
        }
    }
}

fn shorten_session_tree_home(path: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_owned()
    }
}

pub(crate) async fn open_headless_prompt_session(
    options: &PromptRuntimeOptions,
) -> Result<CodingAgentSession, CodingSessionError> {
    let Some(session_options) = options.session.as_ref() else {
        ensure_non_persistent_target(options.session_target.as_ref())?;
        return CodingAgentSession::non_persistent_internal(with_ai_client(
            CodingAgentSessionOptions::new(),
            options.ai_client.as_ref(),
        ))
        .await;
    };
    if !matches!(session_options.mode, SessionMode::Enabled) {
        ensure_non_persistent_target(options.session_target.as_ref())?;
        return CodingAgentSession::non_persistent_internal(with_ai_client(
            session_options_for_run(session_options),
            options.ai_client.as_ref(),
        ))
        .await;
    }

    let session_root = headless_session_root(session_options)?;
    let session_options = with_ai_client(
        session_options_for_run(session_options).with_session_log_root(session_root),
        options.ai_client.as_ref(),
    );
    open_persistent_session(session_options, options.session_target.as_ref()).await
}

pub(crate) fn runtime_session_root(
    options: &SessionRunOptions,
) -> Result<Option<PathBuf>, CodingSessionError> {
    if matches!(options.mode, SessionMode::Enabled) {
        headless_session_root(options).map(Some)
    } else {
        Ok(None)
    }
}

fn target_looks_like_rust_native_session_dir(target: &str) -> bool {
    let path = Path::new(target);
    path.is_dir() && path.join("session.json").is_file() && path.join("events.jsonl").is_file()
}

fn target_looks_like_legacy_jsonl(target: &str) -> bool {
    let path = Path::new(target);
    path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") || path.is_file()
}

pub(crate) fn hydrate_interactive_session_target(
    session_options: &Option<SessionRunOptions>,
    target: Option<&ResolvedSessionTarget>,
) -> Result<Option<CodingAgentSessionSnapshot>, ApplicationError> {
    let Some(session_options) = enabled_session_options(session_options) else {
        return Ok(None);
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let base_options = interactive_navigation_options(session_options)?;
    let hydration = match target {
        ResolvedSessionTarget::New | ResolvedSessionTarget::ForkTarget(_) => return Ok(None),
        ResolvedSessionTarget::ContinueMostRecent => {
            list_interactive_session_hydrations(&Some(session_options.clone()))?
                .into_iter()
                .next()
        }
        ResolvedSessionTarget::OpenOrCreateId(session_id) => {
            match CodingAgentSession::hydrate(base_options.with_session_id(session_id.clone())) {
                Ok(hydration) => Some(hydration),
                Err(_) => return Ok(None),
            }
        }
        ResolvedSessionTarget::OpenTarget(target) => {
            let is_path = target_looks_like_rust_native_session_dir(target);
            let options = if is_path {
                base_options.with_session_path(target)
            } else {
                base_options.with_session_id(target.clone())
            };
            match CodingAgentSession::hydrate(options) {
                Ok(hydration) => Some(hydration),
                Err(error) if is_path => {
                    return Err(ApplicationError::SessionFailure(error.to_string()));
                }
                Err(_) => return Ok(None),
            }
        }
    };
    Ok(hydration
        .filter(|hydration| hydration_matches_cwd(hydration, &session_options.cwd))
        .map(session_snapshot_from_hydration))
}

pub(crate) fn list_interactive_session_hydrations(
    session_options: &Option<SessionRunOptions>,
) -> Result<Vec<CodingAgentSessionHydration>, ApplicationError> {
    let Some(session_options) = enabled_session_options(session_options) else {
        return Ok(Vec::new());
    };
    let options = interactive_navigation_options(session_options)?;
    Ok(CodingAgentSession::list_internal(options.clone())?
        .into_iter()
        .filter_map(|summary| {
            CodingAgentSession::hydrate(options.clone().with_session_id(summary.session_id)).ok()
        })
        .filter(|hydration| hydration_matches_cwd(hydration, &session_options.cwd))
        .collect())
}

fn enabled_session_options(
    session_options: &Option<SessionRunOptions>,
) -> Option<&SessionRunOptions> {
    session_options
        .as_ref()
        .filter(|options| matches!(options.mode, SessionMode::Enabled))
}

fn interactive_navigation_options(
    session_options: &SessionRunOptions,
) -> Result<CodingAgentSessionOptions, CodingSessionError> {
    Ok(session_options_for_run(session_options)
        .with_session_log_root(headless_session_root(session_options)?))
}

fn session_options_for_run(options: &SessionRunOptions) -> CodingAgentSessionOptions {
    match options.workspace.as_ref() {
        Some(workspace) => {
            CodingAgentSessionOptions::new().with_resolved_workspace(workspace.clone())
        }
        None => CodingAgentSessionOptions::new().with_cwd(options.cwd.clone()),
    }
}

fn hydration_matches_cwd(hydration: &CodingAgentSessionHydration, cwd: &Path) -> bool {
    let expected = normalized_path_string(cwd);
    hydration.cwd.as_deref() == Some(expected.as_str())
}

fn normalized_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn ensure_non_persistent_target(
    target: Option<&ResolvedSessionTarget>,
) -> Result<(), CodingSessionError> {
    match target {
        None | Some(ResolvedSessionTarget::New) => Ok(()),
        Some(_) => Err(CodingSessionError::UnsupportedCapability {
            capability: "persistent session target in non-persistent headless mode".into(),
        }),
    }
}

async fn open_persistent_session(
    options: CodingAgentSessionOptions,
    target: Option<&ResolvedSessionTarget>,
) -> Result<CodingAgentSession, CodingSessionError> {
    match target.unwrap_or(&ResolvedSessionTarget::New) {
        ResolvedSessionTarget::New => CodingAgentSession::create_internal(options).await,
        ResolvedSessionTarget::OpenTarget(session_id) => {
            CodingAgentSession::open_internal(options.with_session_id(session_id.clone())).await
        }
        ResolvedSessionTarget::OpenOrCreateId(session_id) => {
            CodingAgentSession::open_or_create_internal(options.with_session_id(session_id.clone()))
                .await
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
            CodingAgentSession::open_internal(options.with_session_id(forked.summary.session_id))
                .await
        }
    }
}

fn headless_session_root(options: &SessionRunOptions) -> Result<PathBuf, CodingSessionError> {
    match options.session_dir.as_ref() {
        Some(root) => Ok(root.clone()),
        None => resolve_session_dir(&options.cwd, None, None).map_err(|error| {
            CodingSessionError::Session {
                message: error.to_string(),
            }
        }),
    }
}

fn with_ai_client(
    options: CodingAgentSessionOptions,
    ai_client: Option<&AiClient>,
) -> CodingAgentSessionOptions {
    match ai_client {
        Some(ai_client) => options.with_ai_client(ai_client.clone()),
        None => options,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::api::transcript::StoredAgentMessage;
    use ai::api::conversation::ContentBlock;

    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_initial_session_name_is_persisted_for_new_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let sessions = temp.path().join("sessions");
        std::fs::create_dir_all(&cwd).unwrap();
        let mut run_options = SessionRunOptions::enabled(cwd);
        run_options.session_dir = Some(sessions);
        let bootstrap = CodingAgentSessionBootstrap::from_internal(
            Some(run_options),
            Some(ResolvedSessionTarget::New),
            Some(" Initial session ".into()),
            ProfileId::from("default"),
        );

        let session = bootstrap.open_internal().await.unwrap();
        let snapshot = session.current_session_snapshot().unwrap().unwrap();

        assert_eq!(snapshot.choice.name.as_deref(), Some("Initial session"));
    }

    async fn create_query_fixture(root: &Path, session_id: &str) {
        let session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_cwd(PathBuf::from("/query-fixture"))
                .with_session_id(session_id)
                .with_session_log_root(root)
                .with_default_agent_profile_id(ProfileId::from("default")),
        )
        .await
        .expect("query fixture session is created");
        drop(session);
    }

    #[test]
    fn default_sessions_root_uses_evo_dir() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let dir = tempfile::tempdir().unwrap();
        env.set_evo_dir(dir.path());

        let root = default_sessions_root().unwrap();

        assert_eq!(root, dir.path().join("sessions"));
        assert!(
            !root.display().to_string().contains(".evo/agent"),
            "default sessions root must not use the legacy ~/.evo tree: {}",
            root.display()
        );
    }

    #[test]
    fn resolve_session_dir_ignores_legacy_agent_dir() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR", "agent_DIR", "EVO_SESSION_DIR"]);
        let global = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();
        env.set_evo_dir(global.path());
        env.set("agent_DIR", legacy.path());
        env.remove("EVO_SESSION_DIR");

        let root = resolve_session_dir(Path::new("."), None, None).unwrap();

        assert_eq!(root, global.path().join("sessions"));
    }

    #[tokio::test]
    async fn global_session_query_lists_two_sessions_without_embedding_context() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR", "EVO_SESSION_DIR"]);
        let global = tempfile::tempdir().unwrap();
        env.set_evo_dir(global.path());
        env.remove("EVO_SESSION_DIR");
        let session_root = global.path().join("sessions");
        create_query_fixture(&session_root, "sess_global_query_a").await;
        create_query_fixture(&session_root, "sess_global_query_b").await;

        let query = CodingAgentSessionQuery::global().expect("global query resolves product root");
        let catalog = query.catalog().expect("global catalog is readable");
        let overviews = query.overviews().expect("global overviews are readable");
        let ids = catalog
            .choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            ids,
            std::collections::BTreeSet::from(["sess_global_query_a", "sess_global_query_b"])
        );
        assert!(!catalog.truncated);
        assert_eq!(overviews.overviews.len(), 2);
        assert!(!overviews.truncated);
        let options = query.options.as_ref().expect("global query is persistent");
        assert_eq!(options.cwd(), None);
        assert_eq!(
            options.default_agent_profile_id(),
            Some(&ProfileId::from("default"))
        );
        let (bounded, truncated) =
            CodingAgentSession::list_overviews_internal(options.clone(), 1).unwrap();
        assert_eq!(bounded.len(), 1);
        assert!(truncated);
    }

    #[tokio::test]
    async fn evo_session_dir_overrides_the_default_global_query_root() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR", "EVO_SESSION_DIR"]);
        let global = tempfile::tempdir().unwrap();
        let override_root = tempfile::tempdir().unwrap();
        env.set_evo_dir(global.path());
        create_query_fixture(&global.path().join("sessions"), "sess_default_root_hidden").await;
        create_query_fixture(override_root.path(), "sess_override_root_visible").await;
        env.set("EVO_SESSION_DIR", override_root.path());

        let catalog = CodingAgentSessionQuery::global()
            .expect("override query resolves")
            .catalog()
            .expect("override catalog is readable");

        assert_eq!(catalog.choices.len(), 1);
        assert_eq!(catalog.choices[0].id, "sess_override_root_visible");
    }

    #[tokio::test]
    async fn session_overviews_match_full_catalog_without_replaying_transcripts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_cwd(PathBuf::from("/overview-project"))
                .with_session_id("sess_overview_fields")
                .with_session_name("Overview name")
                .with_session_log_root(&root)
                .with_default_agent_profile_id(ProfileId::from("default")),
        )
        .await
        .unwrap();
        drop(session);
        let query = CodingAgentSessionQuery::from_session_root(&root);

        let overviews = query.overviews().unwrap();
        let catalog = query.catalog().unwrap();

        assert!(!overviews.truncated);
        assert_eq!(overviews.overviews.len(), 1);
        assert_eq!(catalog.choices.len(), 1);
        let overview = &overviews.overviews[0];
        let choice = &catalog.choices[0];
        assert_eq!(overview.session_id, choice.id);
        assert_eq!(overview.name, choice.name);
        assert_eq!(
            overview.workspace.kind,
            crate::workspace::CodingAgentWorkspaceKind::Project
        );
        assert_eq!(
            overview.workspace_migration.outcome,
            crate::workspace::CodingAgentWorkspaceMigrationOutcome::NotRequired
        );
        assert_eq!(
            overview.workspace.display_path.as_deref(),
            Some(Path::new("/overview-project"))
        );
        assert_eq!(overview.created_at, choice.created_at);
        assert_eq!(overview.updated_at, choice.updated_at);
        assert_eq!(overview.active_leaf_id, choice.active_leaf_id);
        assert_eq!(overview.name.as_deref(), Some("Overview name"));
    }

    #[tokio::test]
    async fn session_overviews_read_only_the_first_bounded_creation_frame() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        create_query_fixture(&root, "sess_bad_later_frame").await;
        create_query_fixture(&root, "sess_bad_first_frame").await;
        downgrade_query_fixture_manifest_to_v1(&root, "sess_bad_later_frame");
        downgrade_query_fixture_manifest_to_v1(&root, "sess_bad_first_frame");
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("sess_bad_later_frame/events.jsonl"))
            .unwrap()
            .write_all(b"not-a-durable-frame\n")
            .unwrap();
        std::fs::write(
            root.join("sess_bad_first_frame/events.jsonl"),
            b"not-a-durable-frame\n",
        )
        .unwrap();
        let query = CodingAgentSessionQuery::from_session_root(&root);

        let overviews = query.overviews().unwrap();
        let catalog = query.catalog().unwrap();

        assert_eq!(overviews.overviews.len(), 1);
        assert_eq!(overviews.overviews[0].session_id, "sess_bad_later_frame");
        assert_eq!(
            overviews.overviews[0].workspace.kind,
            crate::workspace::CodingAgentWorkspaceKind::Project
        );
        assert_eq!(
            overviews.overviews[0].workspace_migration.outcome,
            crate::workspace::CodingAgentWorkspaceMigrationOutcome::Pending
        );
        assert!(
            catalog.choices.is_empty(),
            "full catalog hydration must still reject both corrupt event logs"
        );
    }

    #[tokio::test]
    async fn v2_session_overview_uses_manifest_without_reading_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        create_query_fixture(&root, "sess_manifest_workspace").await;
        std::fs::write(
            root.join("sess_manifest_workspace/events.jsonl"),
            b"not-a-durable-frame\n",
        )
        .unwrap();
        let query = CodingAgentSessionQuery::from_session_root(&root);

        let overviews = query.overviews().unwrap();
        let catalog = query.catalog().unwrap();

        assert_eq!(overviews.overviews.len(), 1);
        assert_eq!(overviews.overviews[0].session_id, "sess_manifest_workspace");
        assert_eq!(
            overviews.overviews[0].workspace.kind,
            crate::workspace::CodingAgentWorkspaceKind::Project
        );
        assert_eq!(
            overviews.overviews[0].workspace_migration.outcome,
            crate::workspace::CodingAgentWorkspaceMigrationOutcome::NotRequired
        );
        assert!(catalog.choices.is_empty());
    }

    #[tokio::test]
    async fn explicit_workspace_migration_upgrades_a_legacy_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        create_query_fixture(&root, "sess_explicit_migration").await;
        downgrade_query_fixture_manifest_to_v1(&root, "sess_explicit_migration");
        let query = CodingAgentSessionQuery::from_session_root(&root);

        let migration = query.migrate_workspace("sess_explicit_migration").unwrap();

        assert_eq!(
            migration.outcome,
            crate::workspace::CodingAgentWorkspaceMigrationOutcome::Migrated
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("sess_explicit_migration/session.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["version"], 2);
        assert_eq!(manifest["workspace_scope"]["kind"], "project");
        assert_eq!(manifest["workspace_migrated_from_legacy"], true);
    }

    #[tokio::test]
    async fn open_target_returns_complete_projectless_scope() {
        let env = crate::test_support::EnvGuard::new(&["EVO_DIR"]);
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let scratch = global.join("scratch/workspace-query-target");
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(&scratch).unwrap();
        env.set_evo_dir(&global);
        let workspace =
            crate::workspace::CodingAgentWorkspaceSelection::projectless("workspace-query-target")
                .resolve(&global)
                .unwrap();
        let session = CodingAgentSession::create_internal(
            CodingAgentSessionOptions::new()
                .with_resolved_workspace(workspace)
                .with_session_id("sess_open_target")
                .with_session_log_root(&root)
                .with_default_agent_profile_id(ProfileId::from("default")),
        )
        .await
        .unwrap();
        drop(session);
        let query = CodingAgentSessionQuery::from_session_root(&root);

        let target = query.open_target("sess_open_target").unwrap();

        assert_eq!(target.session_id, "sess_open_target");
        assert_eq!(
            target.workspace_scope,
            crate::workspace::CodingAgentWorkspaceScope::Projectless {
                workspace_id: "workspace-query-target".into(),
            }
        );
        assert_eq!(
            target.workspace_migration.outcome,
            crate::workspace::CodingAgentWorkspaceMigrationOutcome::NotRequired
        );
        assert_eq!(target.workspace_migration.diagnostic, None);
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("sess_open_target/session.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["workspace_scope"]["kind"], "projectless");
        assert_eq!(
            manifest["workspace_scope"]["workspace_id"],
            "workspace-query-target"
        );
    }

    fn downgrade_query_fixture_manifest_to_v1(root: &Path, session_id: &str) {
        let path = root.join(session_id).join("session.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["version"] = serde_json::json!(1);
        value.as_object_mut().unwrap().remove("workspace_scope");
        value
            .as_object_mut()
            .unwrap()
            .remove("workspace_migrated_from_legacy");
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    #[test]
    fn explicit_session_root_query_is_read_only_when_the_root_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let missing_root = temp.path().join("not-created");
        let query = CodingAgentSessionQuery::from_session_root(&missing_root);

        let catalog = query
            .catalog()
            .expect("missing root projects an empty catalog");

        assert!(catalog.choices.is_empty());
        assert!(!catalog.truncated);
        assert!(!missing_root.exists());
    }

    #[test]
    fn disabled_session_overviews_are_empty() {
        let catalog = CodingAgentSessionQuery::disabled().overviews().unwrap();

        assert!(catalog.overviews.is_empty());
        assert!(!catalog.truncated);
    }

    #[test]
    fn session_tree_query_projects_bounded_authority_free_nodes() {
        let raw_canary = "tree-content-canary ".repeat(40);
        let entry = SessionEntry::message(
            "entry_user".into(),
            None,
            "2026-07-25T00:00:00Z".into(),
            StoredAgentMessage::User {
                content: vec![ContentBlock::Text {
                    text: raw_canary.clone(),
                    text_signature: None,
                }],
                timestamp: 0,
            },
        );
        let tree = CodingAgentSessionTree {
            tree: vec![SessionTreeNode {
                entry,
                children: Vec::new(),
                label: Some("checkpoint".into()),
                label_timestamp: Some("2026-07-25T00:00:01Z".into()),
            }],
            active_leaf_id: Some("entry_user".into()),
        };

        let projected = project_session_tree(tree).unwrap();

        assert_eq!(projected.active_leaf_id.as_deref(), Some("entry_user"));
        assert_eq!(projected.roots.len(), 1);
        let node = &projected.roots[0];
        assert_eq!(node.entry_id, "entry_user");
        assert_eq!(node.message_role, Some(CodingAgentSessionTreeRole::User));
        assert_eq!(node.label.as_deref(), Some("checkpoint"));
        assert!(node.display_text.starts_with("user: tree-content-canary"));
        assert!(
            node.display_text.chars().count() <= MAX_SESSION_TREE_PREVIEW_CHARS + "user: ".len()
        );
        assert!(!format!("{projected:?}").contains(&raw_canary));
    }
}
