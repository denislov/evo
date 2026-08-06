//! MCP OAuth：RFC 8628 device flow（设备码）与 401 后单次 refresh/retry。
//!
//! 与 xai-grok-mcp 的浏览器授权码流程不同：Evo 面向 CLI 环境，采用
//! **device flow**（用户被引导到 verification URI 输入 code，无需本机
//! callback server）。参考 grok `oauth.rs` 的组织（超时预算、轮询、
//! 结构化失败），流程重写：
//!
//! 1. [`device_flow_authenticate`]：POST `device_authorization_endpoint`
//!    （form：`client_id` + `scope`）→ 得 `device_code` / `user_code` /
//!    `verification_uri` / `interval` → 以 interval 轮询 token endpoint
//!    直到成功 / 拒绝 / 超时 / 取消。
//! 2. [`refresh_access_token`]：`refresh_token` 换新 token。
//! 3. 401 retry 语义（[`retry_with_credentials`]）：tools/call 返回 401
//!    → 有 refresh token 则 refresh 一次并重试一次；refresh 失败或仍 401
//!    → 配置了 device flow 则触发一次（幂等，用户可见 URL）；**不无限
//!    重试**——每次失败都是结构化错误返回。
//!
//! 可测性：`poll_interval` 与 HTTP 客户端可注入；mock 端点用本地
//! TcpListener（见测试）。

// Adapted from xai-grok-mcp, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// oauth.rs orchestration budget and failure discipline consulted; RFC 8628
// device flow and single-refresh retry policy are Evo's own.
use std::time::Duration;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::mcp::credentials::McpCredentials;

/// device flow 配置（来自 MCP server 配置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    pub scopes: Vec<String>,
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

/// OAuth 错误分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    #[error("OAuth device authorization failed: {0}")]
    DeviceAuthorization(String),
    #[error("OAuth token exchange failed: {0}")]
    TokenExchange(String),
    #[error("OAuth flow timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("OAuth flow cancelled")]
    Cancelled,
    #[error("OAuth HTTP error: {0}")]
    Http(String),
}

/// 设备授权响应（RFC 8628 §3.2）。
#[derive(Debug, Clone, Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_expires_in() -> u64 {
    900
}

fn default_interval() -> u64 {
    5
}

/// 轮询 token endpoint 的响应。
#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    /// pending / error 响应中不存在；成功时必填。
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// 用户可见提示回调：展示 verification URI 与 user code。
pub type VerificationPresenter = dyn Fn(&str, &str) + Send + Sync;

/// 运行时环境（测试注入 mock HTTP / 快速轮询）。
#[derive(Clone)]
pub struct OAuthRuntime {
    pub client: reqwest::Client,
    pub poll_interval: Duration,
    /// 整体 device flow 预算（用户完成验证的最长等待）。
    pub flow_timeout: Duration,
    /// 用户可见提示回调（默认打印 verification URI）。
    pub present_verification: Option<std::sync::Arc<VerificationPresenter>>,
}

impl std::fmt::Debug for OAuthRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthRuntime")
            .field("poll_interval", &self.poll_interval)
            .field("flow_timeout", &self.flow_timeout)
            .field(
                "has_present_verification",
                &self.present_verification.is_some(),
            )
            .finish()
    }
}

impl Default for OAuthRuntime {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            poll_interval: Duration::from_secs(5),
            flow_timeout: Duration::from_secs(600),
            present_verification: None,
        }
    }
}

/// 运行 RFC 8628 device flow，返回新凭据。
///
/// `present_verification`（或默认打印）向用户展示 verification URI 与
/// code；轮询在 `flow_timeout` 内完成或失败。取消令牌触发即时返回。
pub async fn device_flow_authenticate(
    config: &OAuthConfig,
    runtime: &OAuthRuntime,
    cancel: &CancellationToken,
) -> Result<McpCredentials, OAuthError> {
    let form: Vec<(String, String)> =
        std::iter::once(("client_id".to_string(), config.client_id.clone()))
            .chain(
                config
                    .scopes
                    .iter()
                    .map(|scope| ("scope".to_string(), scope.clone())),
            )
            .collect();
    let response = runtime
        .client
        .post(&config.device_authorization_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|error| OAuthError::Http(error.to_string()))?;
    if !response.status().is_success() {
        return Err(OAuthError::DeviceAuthorization(format!(
            "http status {}",
            response.status().as_u16()
        )));
    }
    let body: DeviceAuthorization = response
        .json()
        .await
        .map_err(|error| OAuthError::DeviceAuthorization(error.to_string()))?;

    match runtime.present_verification.as_ref() {
        Some(present) => present(&body.verification_uri, &body.user_code),
        None => println!(
            "MCP OAuth: open {} and enter code {}",
            body.verification_uri, body.user_code
        ),
    }

    let interval = Duration::from_secs(body.interval.max(1)).max(runtime.poll_interval);
    // 设备码有效期（RFC 8628 §3.2 expires_in）封顶整体预算。
    let deadline = tokio::time::Instant::now()
        + runtime
            .flow_timeout
            .min(Duration::from_secs(body.expires_in));
    loop {
        if cancel.is_cancelled() {
            return Err(OAuthError::Cancelled);
        }
        let form = [
            ("client_id", config.client_id.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", body.device_code.as_str()),
        ];
        let response = runtime
            .client
            .post(&config.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|error| OAuthError::Http(error.to_string()))?;
        let status = response.status().as_u16();
        let raw = response
            .text()
            .await
            .map_err(|error| OAuthError::TokenExchange(error.to_string()))?;
        let body: TokenResponse = serde_json::from_str(&raw)
            .map_err(|error| OAuthError::TokenExchange(format!("{error}: {raw}")))?;
        if let Some(error) = body.error {
            match error.as_str() {
                "authorization_pending" => {}
                "slow_down" => {
                    let _ = tokio::time::sleep(Duration::from_secs(5)).await;
                }
                "expired_token" | "access_denied" | "invalid_grant" => {
                    return Err(OAuthError::TokenExchange(format!(
                        "{error}: {}",
                        body.error_description.unwrap_or_default()
                    )));
                }
                other => {
                    return Err(OAuthError::TokenExchange(format!(
                        "{other}: {}",
                        body.error_description.unwrap_or_default()
                    )));
                }
            }
        } else {
            let access_token = body.access_token.ok_or_else(|| {
                OAuthError::TokenExchange("token response lacks access_token".into())
            })?;
            let expires_at = body.expires_in.map(|seconds| now_unix() + seconds);
            return Ok(McpCredentials {
                access_token,
                refresh_token: body.refresh_token,
                expires_at,
            });
        }
        if status == 429 {
            let _ = tokio::time::sleep(interval).await;
        }
        let next = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + interval));
        tokio::select! {
            _ = cancel.cancelled() => return Err(OAuthError::Cancelled),
            _ = next => {}
        }
    }
}

/// 用 refresh token 换取新 access token（RFC 6749 §6）。
pub async fn refresh_access_token(
    config: &OAuthConfig,
    refresh_token: &str,
    runtime: &OAuthRuntime,
    cancel: &CancellationToken,
) -> Result<McpCredentials, OAuthError> {
    let form = [
        ("client_id", config.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    let response = cancel_aware(
        runtime
            .client
            .post(&config.token_endpoint)
            .form(&form)
            .send(),
        cancel,
    )
    .await?
    .map_err(|error| OAuthError::Http(error.to_string()))?;
    if !response.status().is_success() {
        return Err(OAuthError::TokenExchange(format!(
            "refresh rejected with http status {}",
            response.status().as_u16()
        )));
    }
    let body: TokenResponse = response
        .json()
        .await
        .map_err(|error| OAuthError::TokenExchange(error.to_string()))?;
    if let Some(error) = body.error {
        return Err(OAuthError::TokenExchange(format!(
            "{error}: {}",
            body.error_description.unwrap_or_default()
        )));
    }
    let access_token = body
        .access_token
        .ok_or_else(|| OAuthError::TokenExchange("refresh response lacks access_token".into()))?;
    Ok(McpCredentials {
        access_token,
        refresh_token: body.refresh_token.or(Some(refresh_token.to_string())),
        expires_at: body.expires_in.map(|seconds| now_unix() + seconds),
    })
}

async fn cancel_aware<F: std::future::Future>(
    future: F,
    cancel: &CancellationToken,
) -> Result<F::Output, OAuthError> {
    tokio::pin!(future);
    tokio::select! {
        _ = cancel.cancelled() => Err(OAuthError::Cancelled),
        output = future => Ok(output),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 起一个 mock OAuth 端点，返回 (device_authorization, token)。
    fn start_mock(
        behavior: impl Fn(u64) -> serde_json::Value + Send + Sync + 'static,
    ) -> (String, String) {
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let polls_clone = polls.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut stream = _stream;
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            request.extend_from_slice(&buf[..n]);
                            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let count = polls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or("/");
                let body = if path.starts_with("/device") {
                    json!({
                        "device_code": "dc-1",
                        "user_code": "UC-123",
                        "verification_uri": "http://localhost/verify",
                        "expires_in": 900,
                        "interval": 1,
                    })
                } else {
                    behavior(count)
                };
                let body = serde_json::to_vec(&body).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (
            format!("http://{addr}/device"),
            format!("http://{addr}/token"),
        )
    }

    fn runtime_fast() -> OAuthRuntime {
        OAuthRuntime {
            poll_interval: Duration::from_millis(10),
            flow_timeout: Duration::from_secs(10),
            present_verification: Some(std::sync::Arc::new(|_, _| {})),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn device_flow_succeeds_after_pending_polls() {
        let (device, token) = start_mock(|count| {
            if count < 3 {
                json!({"error": "authorization_pending", "error_description": ""})
            } else {
                json!({"access_token": "at-final", "refresh_token": "rt-final", "expires_in": 3600})
            }
        });
        let config = OAuthConfig {
            client_id: "client".into(),
            scopes: vec![],
            device_authorization_endpoint: device,
            token_endpoint: token,
        };
        let credentials =
            device_flow_authenticate(&config, &runtime_fast(), &CancellationToken::new())
                .await
                .unwrap();
        assert_eq!(credentials.access_token, "at-final");
        assert_eq!(credentials.refresh_token.as_deref(), Some("rt-final"));
        assert!(credentials.expires_at.is_some());
    }

    #[tokio::test]
    async fn device_flow_surfaces_denial() {
        let (device, token) =
            start_mock(|_| json!({"error": "access_denied", "error_description": "user said no"}));
        let config = OAuthConfig {
            client_id: "client".into(),
            scopes: vec![],
            device_authorization_endpoint: device,
            token_endpoint: token,
        };
        let error = device_flow_authenticate(&config, &runtime_fast(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("access_denied"));
    }

    #[tokio::test]
    async fn device_flow_obeys_cancellation() {
        let (device, token) = start_mock(|_| {
            std::thread::sleep(Duration::from_millis(200));
            json!({"error": "authorization_pending"})
        });
        let config = OAuthConfig {
            client_id: "client".into(),
            scopes: vec![],
            device_authorization_endpoint: device,
            token_endpoint: token,
        };
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let config = config.clone();
            let runtime = runtime_fast();
            let cancel = cancel.clone();
            async move { device_flow_authenticate(&config, &runtime, &cancel).await }
        });
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("device flow must return promptly on cancellation")
            .expect("device flow task must not panic");
        assert!(matches!(error, Err(OAuthError::Cancelled)));
    }

    #[tokio::test]
    async fn refresh_exchanges_token_and_keeps_rotation() {
        let (device, token) = start_mock(
            |_| json!({"access_token": "at-refreshed", "refresh_token": "rt-rotated", "expires_in": 60}),
        );
        let config = OAuthConfig {
            client_id: "client".into(),
            scopes: vec![],
            device_authorization_endpoint: device,
            token_endpoint: token,
        };
        let credentials = refresh_access_token(
            &config,
            "rt-old",
            &runtime_fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(credentials.access_token, "at-refreshed");
        assert_eq!(credentials.refresh_token.as_deref(), Some("rt-rotated"));
    }

    #[tokio::test]
    async fn refresh_surfaces_idp_rejection() {
        let (device, token) =
            start_mock(|_| json!({"error": "invalid_grant", "error_description": "token revoked"}));
        let config = OAuthConfig {
            client_id: "client".into(),
            scopes: vec![],
            device_authorization_endpoint: device,
            token_endpoint: token,
        };
        let error = refresh_access_token(
            &config,
            "rt-old",
            &runtime_fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalid_grant"));
    }
}
