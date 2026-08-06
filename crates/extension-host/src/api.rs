//! 公开 API 清单：稳定的 facade，实现 owner 留在各自模块。

pub use crate::budget::{BudgetKind, BudgetSnapshot, BudgetTracker, ExtensionBudget};
pub use crate::config::{
    ExtensionConfig, ExtensionConfigLayer, ExtensionSource, merge_config_layers,
};
pub use crate::diagnostic::{
    DiagnosticLevel, DiagnosticRecord, DiagnosticSink, DiagnosticsCollector, NoopDiagnosticSink,
};
pub use crate::discovery::{
    EXTENSION_MANIFEST_FILE, ExtensionManifest, ExtensionRecord, discover_extensions,
    parse_manifest,
};
pub use crate::error::ExtensionError;
pub use crate::event::{
    EXTENSION_EVENT_VERSION, ExtensionEvent, ExtensionEventKind, ExtensionEventPayload,
    SubagentStopPhase,
};
pub use crate::host::{
    ExtensionHost, ExtensionHostHandle, ExtensionHostOptions, ExtensionHostTask, HostExit,
    HostInfoView, HostState, ShutdownReason,
};
pub use crate::trust::{
    CapabilityClaim, CapabilityRisk, EnableRequest, InMemoryTrustStore, TrustDecision, TrustStatus,
    TrustStore, build_enable_request, decide_trust,
};
