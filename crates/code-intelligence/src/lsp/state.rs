//! LSP server 生命周期状态机（纯决策层，transition 表测试钉死）。
//!
//! [`apply_event`] 是 `LspService` actor 驱动的唯一状态转换入口：任何外部
//! 事件（启动 / spawn 完成 / 握手完成 / 传输死亡 / 退避结束 / shutdown）
//! 先过状态机，非法转换显式拒绝（fail closed）。
//!
//! 转换表（`transition_table_is_complete` 测试逐个钉死）：
//!
//! | 当前状态                 | 事件                 | 新状态                    |
//! |--------------------------|----------------------|---------------------------|
//! | Idle                     | Start                | Starting { attempt: 1 }   |
//! | Reconnecting { attempt } | Start                | Starting { attempt }      |
//! | Starting { attempt }     | Spawned              | Initializing { attempt }  |
//! | Starting { attempt }     | SpawnFailed          | Failed                    |
//! | Initializing { a }       | HandshakeDone        | Ready                     |
//! | Initializing { a }       | HandshakeFailed      | Reconnecting { a + 1 }    |
//! | Ready                    | TransportFailed      | Reconnecting { 1 }        |
//! | Ready                    | LivenessFailed       | Reconnecting { 1 }        |
//! | Reconnecting { a }       | BackoffElapsed       | Starting { a }            |
//! | Reconnecting { a }       | GiveUp               | Failed                    |
//! | 任意（非终态）           | Shutdown             | ShuttingDown              |
//! | ShuttingDown             | StopComplete         | Stopped                   |
//!
//! 与 MCP（`extension-host`）状态机差异：MCP 的握手失败不重试（
//! `Failed`）；LSP 的 spawn 成功后的任何失败（握手失败 / liveness 失败 /
//! 崩溃）都进入 `Reconnecting` 指数退避重试——语言服务器启动期不稳定是
//! 常态，重试到 [`LspServerConfig::max_restart_attempts`] 上限后
//! `GiveUp` 进入 `Failed` 终态。只有 spawn 失败（进程未创建，如二进制
//! 不存在）直接 `Failed` 不重试。
//!
//! [`LspServerConfig::max_restart_attempts`]: crate::lsp::server::LspServerConfig::max_restart_attempts

// Evo 独立设计：状态机为 Evo 自研（MCP 的 ServerLifecycleState 形状
// 参照，但 LSP 语义——document replay、restart 而非 reconnect、GiveUp
// 上限——按 LSP 场景重写）。
use std::time::Duration;

/// LSP server 生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspLifecycleState {
    /// 尚未启动。
    Idle,
    /// 正在 spawn 进程（携带第几次启动尝试）。
    Starting { attempt: u32 },
    /// 进程已 spawn，正在 initialize 握手 + 打开已知 documents。
    Initializing { attempt: u32 },
    /// 握手完成，文档已重放，可服务查询。
    Ready,
    /// 传输失败 / 崩溃 / liveness 失败后退避等待重启（携带第几次重试）。
    Reconnecting { attempt: u32 },
    /// 初始 spawn 失败（不重试）或重试次数用尽。
    Failed { reason: String },
    /// shutdown 中。
    ShuttingDown,
    /// 已终止。
    Stopped,
}

impl LspLifecycleState {
    pub fn is_ready(&self) -> bool {
        matches!(self, LspLifecycleState::Ready)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            LspLifecycleState::Failed { .. } | LspLifecycleState::Stopped
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LspLifecycleState::Idle => "idle",
            LspLifecycleState::Starting { .. } => "starting",
            LspLifecycleState::Initializing { .. } => "initializing",
            LspLifecycleState::Ready => "ready",
            LspLifecycleState::Reconnecting { .. } => "reconnecting",
            LspLifecycleState::Failed { .. } => "failed",
            LspLifecycleState::ShuttingDown => "shutting_down",
            LspLifecycleState::Stopped => "stopped",
        }
    }
}

/// 驱动状态机的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspEvent {
    /// 开始一次启动（首次或重启后）。
    Start,
    /// 进程已 spawn，进入握手。
    Spawned,
    /// spawn 失败（进程未创建，不重试）。
    SpawnFailed,
    /// initialize 握手完成。
    HandshakeDone,
    /// 握手失败（进程活着但握手没成）。
    HandshakeFailed,
    /// Ready 下传输死亡（进程退出 / 读循环终止 / 坏帧 fail closed）。
    TransportFailed,
    /// Ready 下 liveness ping 超时。
    LivenessFailed,
    /// 退避计时结束，开始下一次启动。
    BackoffElapsed,
    /// 重试次数用尽，放弃。
    GiveUp,
    /// shutdown 请求。
    Shutdown,
    /// shutdown 流程完成。
    StopComplete,
}

/// 非法状态转换（fail closed）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid LSP lifecycle transition: {state:?} + {event:?}")]
pub struct TransitionError {
    pub state: LspLifecycleState,
    pub event: LspEvent,
}

/// 应用一个事件到当前状态。
pub fn apply_event(
    state: LspLifecycleState,
    event: LspEvent,
) -> Result<LspLifecycleState, TransitionError> {
    use LspEvent as E;
    use LspLifecycleState as S;
    let next = match (&state, event) {
        // 启动入口：Idle 首次启动 / Reconnecting 重启（attempt 延续）。
        (S::Idle, E::Start) => S::Starting { attempt: 1 },
        (S::Reconnecting { attempt }, E::Start) => S::Starting { attempt: *attempt },
        // spawn 推进。
        (S::Starting { attempt }, E::Spawned) => S::Initializing { attempt: *attempt },
        (S::Starting { attempt }, E::SpawnFailed) => S::Failed {
            reason: format!("spawn failed on attempt {attempt}"),
        },
        // 握手推进。
        (S::Initializing { .. }, E::HandshakeDone) => S::Ready,
        (S::Initializing { attempt }, E::HandshakeFailed) => S::Reconnecting {
            attempt: attempt.saturating_add(1),
        },
        // Ready 下任何失败都进入重连（attempt 1）。
        (S::Ready, E::TransportFailed) | (S::Ready, E::LivenessFailed) => {
            S::Reconnecting { attempt: 1 }
        }
        // 退避结束进入下一次启动。
        (S::Reconnecting { attempt }, E::BackoffElapsed) => S::Starting { attempt: *attempt },
        (S::Reconnecting { .. }, E::GiveUp) => S::Failed {
            reason: "restart attempts exhausted".into(),
        },
        // shutdown 是全局终态。
        (_, E::Shutdown) => S::ShuttingDown,
        (S::ShuttingDown, E::StopComplete) => S::Stopped,
        _ => return Err(TransitionError { state, event }),
    };
    Ok(next)
}

/// 退避时长：`initial * 2^(attempt-1)`，封顶 `max`。attempt 以 1 开始。
pub fn backoff_for(attempt: u32, initial: Duration, max: Duration) -> Duration {
    let exp = attempt.saturating_sub(1).min(8);
    initial.saturating_mul(1u32 << exp).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use LspLifecycleState as S;

    /// transition 表：每个合法转换逐个钉死（与模块文档表一致）。
    /// `Failed` 的 reason 由转换生产（不比较具体文本，只断言变体）。
    #[test]
    fn transition_table_is_complete() {
        let cases: &[(&str, S, LspEvent, S)] = &[
            ("boot", S::Idle, LspEvent::Start, S::Starting { attempt: 1 }),
            (
                "restart after backoff",
                S::Reconnecting { attempt: 3 },
                LspEvent::Start,
                S::Starting { attempt: 3 },
            ),
            (
                "spawned",
                S::Starting { attempt: 2 },
                LspEvent::Spawned,
                S::Initializing { attempt: 2 },
            ),
            (
                "handshake done",
                S::Initializing { attempt: 4 },
                LspEvent::HandshakeDone,
                S::Ready,
            ),
            (
                "handshake failure starts reconnect",
                S::Initializing { attempt: 2 },
                LspEvent::HandshakeFailed,
                S::Reconnecting { attempt: 3 },
            ),
            (
                "transport loss starts reconnect",
                S::Ready,
                LspEvent::TransportFailed,
                S::Reconnecting { attempt: 1 },
            ),
            (
                "liveness failure starts reconnect",
                S::Ready,
                LspEvent::LivenessFailed,
                S::Reconnecting { attempt: 1 },
            ),
            (
                "backoff elapsed restarts",
                S::Reconnecting { attempt: 2 },
                LspEvent::BackoffElapsed,
                S::Starting { attempt: 2 },
            ),
            (
                "shutdown from idle",
                S::Idle,
                LspEvent::Shutdown,
                S::ShuttingDown,
            ),
            (
                "shutdown from ready",
                S::Ready,
                LspEvent::Shutdown,
                S::ShuttingDown,
            ),
            (
                "shutdown from reconnecting",
                S::Reconnecting { attempt: 5 },
                LspEvent::Shutdown,
                S::ShuttingDown,
            ),
            (
                "shutdown from failed",
                S::Failed {
                    reason: String::new(),
                },
                LspEvent::Shutdown,
                S::ShuttingDown,
            ),
            (
                "stop complete",
                S::ShuttingDown,
                LspEvent::StopComplete,
                S::Stopped,
            ),
        ];
        for (label, from, event, expected) in cases {
            assert_eq!(
                apply_event(from.clone(), *event),
                Ok(expected.clone()),
                "transition: {label}"
            );
        }
        // Failed 产物变体断言（reason 由转换生产）。
        assert!(matches!(
            apply_event(S::Starting { attempt: 1 }, LspEvent::SpawnFailed),
            Ok(S::Failed { .. })
        ));
        assert!(matches!(
            apply_event(S::Reconnecting { attempt: 9 }, LspEvent::GiveUp),
            Ok(S::Failed { .. })
        ));
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        // Ready 不能直接 Start（必须先失败进入重连）。
        assert!(matches!(
            apply_event(S::Ready, LspEvent::Start),
            Err(TransitionError { .. })
        ));
        // Idle 不能直接握手。
        assert!(matches!(
            apply_event(S::Idle, LspEvent::Spawned),
            Err(TransitionError { .. })
        ));
        // Starting 不能直接 Ready。
        assert!(matches!(
            apply_event(S::Starting { attempt: 1 }, LspEvent::HandshakeDone),
            Err(TransitionError { .. })
        ));
        // 终态拒绝非 Shutdown 事件。
        assert!(matches!(
            apply_event(
                S::Failed {
                    reason: String::new()
                },
                LspEvent::Start
            ),
            Err(TransitionError { .. })
        ));
        assert!(matches!(
            apply_event(S::Stopped, LspEvent::BackoffElapsed),
            Err(TransitionError { .. })
        ));
        // Stopped 重复 StopComplete 拒绝（幂等由 actor 处理，状态机拒绝）。
        assert!(matches!(
            apply_event(S::Stopped, LspEvent::StopComplete),
            Err(TransitionError { .. })
        ));
    }

    #[test]
    fn repeated_shutdown_is_absorbed() {
        assert_eq!(
            apply_event(S::ShuttingDown, LspEvent::Shutdown).unwrap(),
            S::ShuttingDown
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
