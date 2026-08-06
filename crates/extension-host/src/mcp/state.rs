//! MCP server 生命周期状态机（纯决策层，transition 表测试钉死）。
//!
//! [`apply_event`] 是 `McpServerTask` 驱动的唯一状态转换入口：任何外部
//! 事件（连接 / 握手完成 / 传输失败 / 重连 / shutdown）先过状态机，
//! 非法转换显式拒绝（fail closed）。状态发布经
//! [`crate::mcp::lifecycle::McpHost::server_state`] 查询。
//!
//! 转换表（`transition_table_is_complete` 测试逐个钉死）：
//!
//! | 当前状态         | 事件                | 新状态              |
//! |------------------|---------------------|---------------------|
//! | Disconnected     | Connect             | Connecting          |
//! | Reconnecting     | Connect             | Connecting          |
//! | Connecting       | HandshakeStarted    | Initializing        |
//! | Initializing     | Ready               | Ready               |
//! | Connecting       | ConnectFailed       | Failed              |
//! | Initializing     | ConnectFailed       | Failed              |
//! | Ready            | ConnectFailed       | Reconnecting(1)     |
//! | Failed           | Reconnect           | Reconnecting(1)     |
//! | Reconnecting(n)  | ConnectFailed       | Reconnecting(n+1)   |
//! | 任意             | Shutdown            | Terminated          |
//! | 任意非终态       | Connect             | 合法（重复触发幂等）|
//!
//! 非法转换（如 `Ready + Connect`、`Terminated` 上的任何事件）返回
//! [`TransitionError`]。

// Evo 独立设计：状态机为 Evo 自研（xai-grok-mcp 的 ClientStateKind 只有
// Empty/Pending/Initializing/Ready 四个态且无显式转换表）。
use std::time::Duration;

/// server 生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLifecycleState {
    /// 尚未开始 / 已终止。
    Disconnected,
    /// 正在建立传输（spawn / 连接）。
    Connecting,
    /// 正在 initialize 握手与工具发现。
    Initializing,
    /// initialize 完成，工具可用。
    Ready,
    /// 失败后退避等待重试（携带第几次重试）。
    Reconnecting { attempt: u32 },
    /// 初始连接失败（不重试）。
    Failed { reason: String },
    /// shutdown 中。
    ShuttingDown,
    /// 已终止。
    Terminated,
}

impl ServerLifecycleState {
    pub fn is_ready(&self) -> bool {
        matches!(self, ServerLifecycleState::Ready)
    }
}

/// 驱动状态机的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// 开始建立传输。
    Connect,
    /// 传输就绪，进入握手。
    HandshakeStarted,
    /// 握手 + 工具发现完成。
    Ready,
    /// 传输 / 握手 / liveness 失败。
    ConnectFailed,
    /// 进入退避等待。
    Reconnect,
    /// shutdown。
    Shutdown,
}

/// 非法状态转换（fail closed）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid MCP lifecycle transition: {state:?} + {event:?}")]
pub struct TransitionError {
    pub state: ServerLifecycleState,
    pub event: LifecycleEvent,
}

/// 应用一个事件到当前状态。
pub fn apply_event(
    state: ServerLifecycleState,
    event: LifecycleEvent,
) -> Result<ServerLifecycleState, TransitionError> {
    use LifecycleEvent as E;
    use ServerLifecycleState as S;
    let next = match (&state, event) {
        // 连接入口：Disconnected / Reconnecting / Failed（重试）都可进入。
        (S::Disconnected, E::Connect) | (S::Reconnecting { .. }, E::Connect) => S::Connecting,
        // 重连成功路径：Failed + Reconnect 进入退避。
        (S::Failed { .. }, E::Reconnect) => S::Reconnecting { attempt: 1 },
        // 退避中再失败：attempt +1（由 task 提供当前 attempt，见下方
        // 重载）。
        (S::Reconnecting { attempt }, E::ConnectFailed) => S::Reconnecting {
            attempt: attempt.saturating_add(1),
        },
        // 握手推进。
        (S::Connecting, E::HandshakeStarted) => S::Initializing,
        (S::Connecting, E::ConnectFailed) => S::Failed {
            reason: String::new(),
        },
        (S::Initializing, E::ConnectFailed) => S::Failed {
            reason: String::new(),
        },
        (S::Initializing, E::Ready) => S::Ready,
        // Ready 下任何传输失败都进入重连（attempt 1）。
        (S::Ready, E::ConnectFailed) => S::Reconnecting { attempt: 1 },
        // shutdown 是全局终态。
        (S::ShuttingDown, E::Shutdown) | (S::Terminated, E::Shutdown) => S::Terminated,
        (_, E::Shutdown) => S::Terminated,
        // 重复事件幂等：Connecting 期间的重复 Connect / 重复握手。
        (S::Connecting, E::Connect) => S::Connecting,
        (S::Initializing, E::HandshakeStarted) => S::Initializing,
        (S::Ready, E::Ready) => S::Ready,
        _ => return Err(TransitionError { state, event }),
    };
    Ok(next)
}

/// 退避时长：`initial * 2^(attempt-1)`，封顶 `max`。
pub fn backoff_for(attempt: u32, initial: Duration, max: Duration) -> Duration {
    let exp = attempt.saturating_sub(1).min(8);
    initial.saturating_mul(1u32 << exp).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ServerLifecycleState as S;

    /// transition 表：每个合法转换逐个钉死（与模块文档表一致）。
    #[test]
    fn transition_table_is_complete() {
        let cases: &[(&str, S, LifecycleEvent, S)] = &[
            (
                "boot",
                S::Disconnected,
                LifecycleEvent::Connect,
                S::Connecting,
            ),
            (
                "reconnect enters connecting",
                S::Reconnecting { attempt: 3 },
                LifecycleEvent::Connect,
                S::Connecting,
            ),
            (
                "handshake",
                S::Connecting,
                LifecycleEvent::HandshakeStarted,
                S::Initializing,
            ),
            ("ready", S::Initializing, LifecycleEvent::Ready, S::Ready),
            (
                "initial connect failure",
                S::Connecting,
                LifecycleEvent::ConnectFailed,
                S::Failed {
                    reason: String::new(),
                },
            ),
            (
                "handshake failure",
                S::Initializing,
                LifecycleEvent::ConnectFailed,
                S::Failed {
                    reason: String::new(),
                },
            ),
            (
                "ready loss starts reconnect",
                S::Ready,
                LifecycleEvent::ConnectFailed,
                S::Reconnecting { attempt: 1 },
            ),
            (
                "failed retries",
                S::Failed {
                    reason: String::new(),
                },
                LifecycleEvent::Reconnect,
                S::Reconnecting { attempt: 1 },
            ),
            (
                "reconnect backoff failure escalates attempt",
                S::Reconnecting { attempt: 2 },
                LifecycleEvent::ConnectFailed,
                S::Reconnecting { attempt: 3 },
            ),
            (
                "shutdown from boot",
                S::Disconnected,
                LifecycleEvent::Shutdown,
                S::Terminated,
            ),
            (
                "shutdown from ready",
                S::Ready,
                LifecycleEvent::Shutdown,
                S::Terminated,
            ),
            (
                "shutdown from reconnecting",
                S::Reconnecting { attempt: 9 },
                LifecycleEvent::Shutdown,
                S::Terminated,
            ),
        ];
        for (label, from, event, expected) in cases {
            assert_eq!(
                apply_event(from.clone(), *event),
                Ok(expected.clone()),
                "transition: {label}"
            );
        }
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        // Ready 不能直接 Connect（必须先失败进入重连）。
        assert!(matches!(
            apply_event(S::Ready, LifecycleEvent::Connect),
            Err(TransitionError { .. })
        ));
        // Terminated 是终态：任何非 Shutdown 事件都拒绝。
        assert!(matches!(
            apply_event(S::Terminated, LifecycleEvent::Connect),
            Err(TransitionError { .. })
        ));
        assert!(matches!(
            apply_event(S::Terminated, LifecycleEvent::Ready),
            Err(TransitionError { .. })
        ));
        // 失败态不能直接握手。
        assert!(matches!(
            apply_event(
                S::Failed {
                    reason: String::new()
                },
                LifecycleEvent::HandshakeStarted
            ),
            Err(TransitionError { .. })
        ));
        // 未连接的 Disconnected 不能握手。
        assert!(matches!(
            apply_event(S::Disconnected, LifecycleEvent::Ready),
            Err(TransitionError { .. })
        ));
    }

    #[test]
    fn idempotent_repeats_are_allowed() {
        assert_eq!(
            apply_event(S::Connecting, LifecycleEvent::Connect).unwrap(),
            S::Connecting
        );
        assert_eq!(
            apply_event(S::Initializing, LifecycleEvent::HandshakeStarted).unwrap(),
            S::Initializing
        );
        assert_eq!(
            apply_event(S::Ready, LifecycleEvent::Ready).unwrap(),
            S::Ready
        );
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let initial = Duration::from_millis(100);
        let max = Duration::from_secs(1);
        assert_eq!(backoff_for(1, initial, max), Duration::from_millis(100));
        assert_eq!(backoff_for(2, initial, max), Duration::from_millis(200));
        assert_eq!(backoff_for(3, initial, max), Duration::from_millis(400));
        assert_eq!(backoff_for(4, initial, max), Duration::from_millis(800));
        assert_eq!(backoff_for(5, initial, max), Duration::from_secs(1));
        assert_eq!(backoff_for(100, initial, max), Duration::from_secs(1));
    }
}
