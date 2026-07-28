mod bridge;
mod dispatch;
mod driver;
mod protocol;

#[allow(unused_imports)]
pub use bridge::{
    DesktopRuntimeBootstrap, DesktopRuntimeBridge, DesktopRuntimeCommandHandle,
    DesktopRuntimeEventStream, DesktopRuntimeShutdownGuard,
};
use driver::run_runtime;
#[allow(unused_imports)]
pub use protocol::{
    DESKTOP_COMMAND_QUEUE_CAPACITY, DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY,
    DESKTOP_UPDATE_QUEUE_CAPACITY, DesktopCommandAdmissionError, DesktopRecoveryAction,
    DesktopRecoveryIdentity, DesktopRuntimeCommandKind, DesktopRuntimeError,
    DesktopRuntimeHydratedSnapshot, DesktopRuntimeMetadataSnapshot, DesktopRuntimeRecoverySnapshot,
    DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind, DesktopRuntimeShutdownError,
    DesktopRuntimeStartError, DesktopRuntimeUpdate, DesktopSessionCatalogEntry,
    MAX_DESKTOP_SESSION_CATALOG,
};
#[allow(unused_imports)]
pub use protocol::{MAX_CONTROL_TEXT_BYTES, MAX_PROMPT_BYTES};

#[cfg(test)]
mod tests;
