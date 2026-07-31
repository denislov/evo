//! Stable desktop runtime contract over a client/worker split.

mod client;
mod protocol;
mod worker;

pub(crate) use client::DesktopRuntimeShutdownSignal;
pub use client::{
    DesktopRuntimeBridge, DesktopRuntimeEventStream, DesktopRuntimeShutdownGuard,
    RuntimeCommandClient,
};
pub use protocol::{
    DesktopPromptTarget, DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeCommandKind,
    DesktopRuntimeError, DesktopRuntimeHydratedSnapshot, DesktopRuntimeMetadataSnapshot,
    DesktopRuntimeOwnerTarget, DesktopRuntimeRecoverySnapshot, DesktopRuntimeResyncSnapshot,
    DesktopRuntimeSelectionKind, DesktopRuntimeShutdownError, DesktopRuntimeStartError,
    DesktopRuntimeUpdate, DesktopSessionCatalogEntry, MAX_DESKTOP_SESSION_CATALOG,
    MAX_PROMPT_ATTACHMENTS, validate_prompt_attachments,
};

#[cfg(test)]
mod tests;
