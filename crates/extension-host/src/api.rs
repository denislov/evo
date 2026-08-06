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
pub use crate::dispatcher::{
    HookGate, HookRegistry, StopGateDecision, ToolGateDecision, event_gate,
};
pub use crate::error::ExtensionError;
pub use crate::event::{
    EXTENSION_EVENT_VERSION, ExtensionEvent, ExtensionEventKind, ExtensionEventPayload,
    MAX_HOOK_PAYLOAD_BYTES, SubagentStopPhase, truncate_json_payload,
};
pub use crate::hook::{HookSpec, parse_event, parse_hooks, sort_hooks};
pub use crate::host::{
    ExtensionHost, ExtensionHostHandle, ExtensionHostOptions, ExtensionHostTask, HostExit,
    HostInfoView, HostState, ShutdownReason,
};
pub use crate::matcher::{HookMatcher, MatchContext};
pub use crate::runner::{
    DEFAULT_HOOK_TIMEOUT, EVENT_ENV_VAR, GateKind, HOOK_NAME_ENV_VAR, HOOK_OUTPUT_MAX_BYTES,
    HOOK_OUTPUT_MAX_LINES, HookRunOutcome, RunContext, SESSION_ENV_VAR, StopSignals,
    WORKSPACE_ENV_VAR, run_hook,
};
pub use crate::trust::{
    CapabilityClaim, CapabilityRisk, EnableRequest, InMemoryTrustStore, TrustDecision, TrustStatus,
    TrustStore, build_enable_request, decide_trust,
};
