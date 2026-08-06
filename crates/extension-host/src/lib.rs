//! # extension-host
//!
//! Evo 的外部扩展宿主（Phase 7 / ARC-700 + ARC-710）：在稳定 tool runtime
//! 之上开放外部扩展（user hooks、MCP provider），不污染产品内核。
//!
//! 治理机制与 hooks 派发：
//!
//! - discovery：从 global / project 目录发现扩展 manifest（每个扩展一个目录
//!   加 `extension.json`，manifest 可声明 `hooks` 数组）。
//! - config merge：配置层按 `Managed > Project > Global` 优先级合并，
//!   scalar 高优先级覆盖、`permissions` 并集、`enabled` 任意层可禁用（AND）。
//! - trust：folder trust 单一判定入口（复用产品 folder trust 概念，不建
//!   第二套信任库）；未决定（首次启用）时产出 [`EnableRequest`] 展示 DTO，
//!   由产品决定是否放行。
//! - matcher：[`HookMatcher`] 支持 event/tool/path/profile 四维条件；
//!   [`sort_hooks`] 提供 priority + name 的确定执行顺序。
//! - runner：[`run_hook`] 把 hook 作为沙箱子进程执行（事件经环境变量
//!   注入、输出按预算截断、超时/取消/崩溃有结构化结果）。
//! - gate：[`HookGate`] 提供 Tool / Stop gate 的同步评估（产品在 agent
//!   loop 内调用）；Observe 事件经 host 通道串行派发。
//! - lifecycle / shutdown：[`ExtensionHost::start`] 启动后台 dispatch task，
//!   [`ExtensionHostHandle::shutdown`] 先取消在途 hook 再按确定性顺序关闭
//!   （拒绝新事件 -> drain 已提交事件 -> 诊断收尾），
//!   [`ExtensionHostTask::join`] 回收结果。
//! - budget：每扩展输出字节 / 调用次数 / 运行时长 / 并发上限类型与记账
//!   （run_secs 由 runner 超时强制）。
//! - diagnostics：结构化诊断记录（code / level / context），有界环形缓冲 +
//!   可注入 sink。
//!
//! 版本化事件 DTO：[`event::ExtensionEvent`] 带 `version` 字段（wire 协议
//! 版本），payload 与产品 `ProductEvent` 完全隔离；字段缺失通过
//! `#[serde(default)]` 向后兼容。事件 kind 覆盖 session / prompt / tool /
//! permission / stop / subagent / compaction / merge 八类。
//!
//! 扩展点：`ExtensionHostOptions`（ARC-720 扩展配置字段）、manifest 的
//! `capabilities` 与 `ConcurrentExtensions` 维度（ARC-720 MCP 注册入口）。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// module organization consulted; crate-level design is Evo's own.
pub mod api;
pub mod budget;
pub mod config;
pub mod diagnostic;
pub mod discovery;
pub mod dispatcher;
pub mod error;
pub mod event;
pub mod hook;
pub mod host;
pub mod matcher;
pub mod runner;
pub mod trust;

pub use crate::api::*;
