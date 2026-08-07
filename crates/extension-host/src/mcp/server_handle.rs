//! MCP server 调用句柄：per-server 会话转发、OAuth 凭据恢复与
//! Authorization 动态注入（ARC-730：refresh 后的新 token 立即生效）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::diagnostic::{DiagnosticLevel, DiagnosticRecord, DiagnosticSink};
use crate::mcp::credentials::McpCredentialStore;
use crate::mcp::lifecycle::McpServerConfig;
use crate::mcp::oauth::{OAuthRuntime, device_flow_authenticate, refresh_access_token};
use crate::mcp::transport::{RpcError, RpcSession, TransportConfig};

/// server 的调用句柄（`McpHost` 暴露给 meta tools / 产品）。
#[derive(Debug, Clone)]
pub struct McpServerHandle {
    config: McpServerConfig,
    /// 当前会话（`Ready` 时 `Some`；重连时被替换）。
    session: Arc<tokio::sync::Mutex<Option<Arc<RpcSession>>>>,
    /// host shutdown 时触发：在途调用立即失败。
    host_cancel: CancellationToken,
    credential_store: Arc<dyn McpCredentialStore>,
    oauth_runtime: OAuthRuntime,
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl McpServerHandle {
    /// 构造（lifecycle 装配使用）。
    pub(crate) fn new(
        config: McpServerConfig,
        host_cancel: CancellationToken,
        credential_store: Arc<dyn McpCredentialStore>,
        oauth_runtime: OAuthRuntime,
        diagnostics: Arc<dyn DiagnosticSink>,
    ) -> Self {
        Self {
            config,
            session: Arc::new(tokio::sync::Mutex::new(None)),
            host_cancel,
            credential_store,
            oauth_runtime,
            diagnostics,
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// 会话互斥体访问（lifecycle 状态机使用）。
    pub(crate) fn session(&self) -> Arc<tokio::sync::Mutex<Option<Arc<RpcSession>>>> {
        Arc::clone(&self.session)
    }

    /// 声明配置访问（lifecycle 状态机使用）。
    pub(crate) fn config(&self) -> &McpServerConfig {
        &self.config
    }

    /// 当前凭据派生出的请求头（ARC-730 OAuth 注入）。
    ///
    /// 仅 HTTP transport 且 credential store 有 access token 时返回
    /// `Authorization: Bearer <token>`（动态凭据优先于静态配置的
    /// `Authorization`；无动态凭据时静态 header 原样使用）。stdio
    /// transport 不注入（无 HTTP 请求语义）。
    pub(crate) fn auth_headers(&self) -> Vec<(String, String)> {
        if !matches!(self.config.transport, TransportConfig::Http(_)) {
            return Vec::new();
        }
        self.credential_store
            .get(&self.config.name)
            .map(|credentials| {
                vec![(
                    "authorization".into(),
                    format!("Bearer {}", credentials.access_token),
                )]
            })
            .unwrap_or_default()
    }

    /// 结构化诊断（坏工具 / 重连 / OAuth 恢复失败等）。
    fn record(&self, code: &str, message: impl Into<String>) {
        self.diagnostics.emit(DiagnosticRecord {
            level: DiagnosticLevel::Warning,
            code: code.into(),
            message: message.into(),
            extension_id: Some(self.config.name.clone()),
            context: Default::default(),
        });
    }

    /// 调用服务器工具。401 → 单次 refresh（有 refresh token）或
    /// device flow（配置了 OAuth）后重试一次；失败返回结构化错误。
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let timeout = self.config.tool_timeout;
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let first = self
            .request("tools/call", Some(params.clone()), timeout, cancel)
            .await;
        match first {
            Ok(result) => Ok(result),
            Err(error) if error.is_unauthorized() => {
                if self.try_credential_retry(cancel).await {
                    self.request("tools/call", Some(params), timeout, cancel)
                        .await
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// 请求转发：服务器不在 `Ready`（无会话）时给出结构化错误；host
    /// shutdown（`host_cancel`）时在途调用立即失败。请求携带当前
    /// credential store 派生的 `Authorization`（HTTP transport）。
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let session_guard = self.session.lock().await;
        let Some(session) = session_guard.as_ref() else {
            return Err(RpcError::Other(
                "MCP server is not connected (reconnecting or failed)".into(),
            ));
        };
        // 取 Arc 后立即释放锁：请求等待期间不阻塞重连替换会话。
        let session = Arc::clone(session);
        drop(session_guard);
        let headers = self.auth_headers();
        tokio::select! {
            biased;
            _ = self.host_cancel.cancelled() => Err(RpcError::Cancelled),
            result = session.request_with_headers(method, params, timeout, cancel, &headers) => result,
        }
    }
}

impl McpServerHandle {
    /// 401 后的凭据恢复：refresh 优先，device flow 兜底；成功返回 `true`。
    async fn try_credential_retry(&self, cancel: &CancellationToken) -> bool {
        let Some(oauth) = self.config.oauth.as_ref() else {
            return false;
        };
        if let Some(credentials) = self.credential_store.get(&self.config.name)
            && let Some(refresh_token) = credentials.refresh_token
        {
            match refresh_access_token(oauth, &refresh_token, &self.oauth_runtime, cancel).await {
                Ok(credentials) => {
                    let _ = self.credential_store.set(&self.config.name, credentials);
                    return true;
                }
                Err(error) => {
                    self.record(
                        "mcp_refresh_failed",
                        format!("token refresh failed: {error}; falling back to device flow"),
                    );
                }
            }
        }
        match device_flow_authenticate(oauth, &self.oauth_runtime, cancel).await {
            Ok(credentials) => {
                let _ = self.credential_store.set(&self.config.name, credentials);
                true
            }
            Err(error) => {
                self.record(
                    "mcp_oauth_recovery_failed",
                    format!("OAuth recovery failed: {error}"),
                );
                false
            }
        }
    }
}
