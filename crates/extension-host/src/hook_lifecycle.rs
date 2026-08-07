//! Hook 生命周期观察点（ARC-730 hook 修改归因的注入 seam）。
//!
//! [`dispatch_observe`] 在每个匹配 hook 执行前后调用 [`HookLifecycle`]
//! 的 `before` / `after`（携带事件与 hook spec；`after` 附带运行结果）。
//! 默认实现为 no-op（[`NoopHookLifecycle`]）；产品侧（coding-agent）注入
//! 实现做「hook 修改文件自动归因 `HookEdit`」—— 观察点只负责时机，归因
//! 逻辑与 change-tracker 依赖都留在产品侧，extension-host 不引入对
//! change-tracker 的依赖（依赖图不变）。
//!
//! 失败不阻断：生命周期方法返回 `()`，实现内部自行吞错（观察是
//! best-effort，hook 执行与事件派发不受观察点影响）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::event::ExtensionEvent;
use crate::hook::HookSpec;
use crate::runner::HookRunOutcome;

/// Hook 生命周期观察者。
pub trait HookLifecycle: std::fmt::Debug + Send + Sync {
    /// hook 执行前（matcher 已通过、进程即将启动）。实现可在此采集
    /// 归因基线（如 change-tracker 快照）。
    fn before<'a>(
        &'a self,
        event: &'a ExtensionEvent,
        spec: &'a HookSpec,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    /// hook 执行后（成功或失败都调用）。实现可在此对比基线并归因
    /// （如生成 `HookEdit` receipt）。
    fn after<'a>(
        &'a self,
        event: &'a ExtensionEvent,
        spec: &'a HookSpec,
        outcome: &'a HookRunOutcome,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// 默认 no-op 实现（未注入观察点时行为不变）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHookLifecycle;

impl HookLifecycle for NoopHookLifecycle {
    fn before<'a>(
        &'a self,
        _event: &'a ExtensionEvent,
        _spec: &'a HookSpec,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn after<'a>(
        &'a self,
        _event: &'a ExtensionEvent,
        _spec: &'a HookSpec,
        _outcome: &'a HookRunOutcome,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

/// 便捷构造：`Arc<dyn HookLifecycle>`（host 装配处使用）。
pub fn hook_lifecycle_arc(lifecycle: impl HookLifecycle + 'static) -> Arc<dyn HookLifecycle> {
    Arc::new(lifecycle)
}
