//! Bounded runtime snapshots, catalog entries, selection kinds, and recovery DTOs.

use coding_agent::api::client::{CodingAgentRecoveryPending, CodingAgentSnapshot};
use coding_agent::api::embedding::CodingAgentEmbeddingSnapshot;
use coding_agent::api::view::{
    CodingAgentTranscriptSnapshot, CodingAgentWorkspaceKind, CodingAgentWorkspaceMigration,
    CodingAgentWorkspaceMigrationOutcome, CodingAgentWorkspaceOverview,
};

pub struct DesktopRuntimeReadySnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeHydratedSnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: CodingAgentSnapshot,
    pub transcript: CodingAgentTranscriptSnapshot,
    pub pending_recoveries: Vec<CodingAgentRecoveryPending>,
}

/// Narrow project/session replacement for metadata-only desktop commands.
///
/// This type intentionally cannot carry a transcript or durable recovery
/// payload. Reload and selection commands therefore cannot accidentally
/// hydrate or clone the conversation while refreshing product metadata.
#[derive(Debug, Clone)]
pub struct DesktopRuntimeMetadataSnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: Option<CodingAgentSnapshot>,
}

/// Narrow recovery replacement without durable transcript content.
#[derive(Debug, Clone)]
pub struct DesktopRuntimeRecoverySnapshot {
    pub project: CodingAgentEmbeddingSnapshot,
    pub session: CodingAgentSnapshot,
    pub pending_recoveries: Vec<CodingAgentRecoveryPending>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSessionCatalogEntry {
    pub session_id: String,
    pub name: Option<String>,
    pub workspace: CodingAgentWorkspaceOverview,
    pub workspace_migration: CodingAgentWorkspaceMigration,
    pub created_at: String,
    pub updated_at: String,
    pub active_leaf_id: Option<String>,
}

impl Default for DesktopSessionCatalogEntry {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            name: None,
            workspace: CodingAgentWorkspaceOverview {
                group_id: "legacy:unscoped".into(),
                kind: CodingAgentWorkspaceKind::Legacy,
                display_name: "Legacy session".into(),
                display_path: None,
            },
            workspace_migration: CodingAgentWorkspaceMigration {
                outcome: CodingAgentWorkspaceMigrationOutcome::Unavailable,
                diagnostic: None,
            },
            created_at: String::new(),
            updated_at: String::new(),
            active_leaf_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DesktopRuntimeResyncSnapshot {
    Metadata(DesktopRuntimeMetadataSnapshot),
    Hydrated(DesktopRuntimeHydratedSnapshot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeSelectionKind {
    Model,
    SessionProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRecoveryIdentity {
    pub operation_id: String,
    pub recovery_id: String,
    pub record_version: u64,
    pub descriptor_revision: u16,
    pub capability_generation: Option<u64>,
    pub attempt_count: u32,
}

impl From<&CodingAgentRecoveryPending> for DesktopRecoveryIdentity {
    fn from(pending: &CodingAgentRecoveryPending) -> Self {
        Self {
            operation_id: pending.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
            attempt_count: pending.attempt_count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRecoveryAction {
    Retry,
    MarkFailed,
    Abort,
}
