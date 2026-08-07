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
pub use crate::hook_lifecycle::{HookLifecycle, NoopHookLifecycle};
pub use crate::host::{
    ExtensionHost, ExtensionHostHandle, ExtensionHostOptions, ExtensionHostTask, HostExit,
    HostInfoView, HostState, ShutdownReason,
};
pub use crate::matcher::{HookMatcher, MatchContext};
pub use crate::mcp::credentials::{
    CREDENTIALS_FILENAME, CredentialStoreError, FileCredentialStore, McpCredentialStore,
    McpCredentials,
};
pub use crate::mcp::lifecycle::{
    DEFAULT_PING_INTERVAL, DEFAULT_PING_TIMEOUT, DEFAULT_RECONNECT_BASE, DEFAULT_RECONNECT_MAX,
    DEFAULT_TOOL_TIMEOUT, DiscoveredTool, LivenessConfig, McpHost, McpServerConfig,
    McpServerHandle, ReconnectConfig, convert_tool,
};
pub use crate::mcp::meta::{
    MCP_SEARCH_TOOL_ID, MCP_USE_TOOL_ID, meta_tools, search_tool, use_tool,
};
pub use crate::mcp::oauth::{OAuthConfig, OAuthError, OAuthRuntime};
pub use crate::mcp::transport::{HttpConfig, RpcError, RpcSession, StdioConfig, TransportConfig};
pub use crate::mcp::wire::{
    ErrorResponse, Id, JSONRPC_VERSION, JsonRpcError, Message, Notification, Request, Response,
    WireError, parse_message,
};
pub use crate::mcp::{LifecycleEvent, ServerLifecycleState, apply_event, backoff_for};
pub use crate::runner::{
    DEFAULT_HOOK_TIMEOUT, EVENT_ENV_VAR, GateKind, HOOK_NAME_ENV_VAR, HOOK_OUTPUT_MAX_BYTES,
    HOOK_OUTPUT_MAX_LINES, HookRunOutcome, RunContext, SESSION_ENV_VAR, StopSignals,
    WORKSPACE_ENV_VAR, run_hook,
};
pub use crate::trust::{
    CapabilityClaim, CapabilityRisk, EnableRequest, InMemoryTrustStore, TrustDecision, TrustStatus,
    TrustStore, build_enable_request, decide_trust,
};
