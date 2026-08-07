//! MCP provider adapter（ARC-720）：MCP 作为 external tool provider，
//! 不进入 agent-core。
//!
//! 模块划分：
//!
//! - [`wire`]：MCP JSON-RPC 2.0 wire 类型与严格解析。
//! - [`transport`]：stdio（sandbox 子进程）与 HTTP 两种 transport 上的
//!   JSON-RPC 会话（请求-响应 / 通知 / 超时 / 取消）。
//! - [`credentials`]：credential seam（token 与 refresh token 分离）+
//!   默认文件存储（0600 / 原子写 / 目录可注入）。
//! - [`oauth`]：RFC 8628 device flow 与 401 后单次 refresh/retry。
//! - [`lifecycle`]：server 生命周期状态机（initialize / liveness /
//!   reconnect / tools 热更新 / 确定性 shutdown）与 [`McpHost`] 装配。
//! - [`meta`]：`mcp_search` / `mcp_use` meta tools（DynamicTool）。
//!
//! ACP transport 不在本任务范围（无真实需求，见 master plan 债务）。

pub mod credentials;
pub mod lifecycle;
pub mod meta;
pub mod oauth;
mod server_handle;
mod state;
pub mod transport;
pub mod wire;

pub use credentials::{
    CREDENTIALS_FILENAME, CredentialStoreError, FileCredentialStore, McpCredentialStore,
    McpCredentials,
};
pub use lifecycle::{
    DEFAULT_PING_INTERVAL, DEFAULT_PING_TIMEOUT, DEFAULT_RECONNECT_BASE, DEFAULT_RECONNECT_MAX,
    DEFAULT_TOOL_TIMEOUT, DiscoveredTool, LivenessConfig, McpHost, McpServerConfig,
    McpServerHandle, ReconnectConfig, convert_tool,
};
pub use meta::{MCP_SEARCH_TOOL_ID, MCP_USE_TOOL_ID, meta_tools, search_tool, use_tool};
pub use oauth::{OAuthConfig, OAuthError, OAuthRuntime};
pub use state::{LifecycleEvent, ServerLifecycleState, apply_event, backoff_for};
pub use transport::{HttpConfig, RpcError, RpcSession, StdioConfig, TransportConfig};
pub use wire::{
    ErrorResponse, Id, JSONRPC_VERSION, JsonRpcError, Message, Notification, Request, Response,
    WireError, parse_message,
};
