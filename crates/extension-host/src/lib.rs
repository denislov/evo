//! # extension-host
//!
//! Evo 的外部扩展宿主（Phase 7 / ARC-700）：在稳定 tool runtime 之上开放
//! 外部扩展（user hooks、MCP provider），不污染产品内核。
//!
//! 本 crate 只提供骨架级治理机制，具体扩展业务（事件派发 runner、MCP
//! adapter）由后续 ARC 实现：
//!
//! - discovery：从 global / project 目录发现扩展 manifest（每个扩展一个目录
//!   加 `extension.json`）。
//! - config merge：配置层按 `Managed > Project > Global` 优先级合并，
//!   scalar 高优先级覆盖、`permissions` 并集、`enabled` 任意层可禁用（AND）。
//! - trust：folder trust 单一判定入口（复用产品 folder trust 概念，不建
//!   第二套信任库）；未决定（首次启用）时产出 [`EnableRequest`] 展示 DTO，
//!   由产品决定是否放行。
//! - lifecycle / shutdown：[`ExtensionHost::start`] 启动后台 dispatch task，
//!   [`ExtensionHostHandle::shutdown`] 按确定性顺序关闭（拒绝新事件 ->
//!   drain 已提交事件 -> 诊断收尾），[`ExtensionHostTask::join`] 回收结果。
//! - budget：每扩展输出字节 / 调用次数 / 时长上限类型与记账。
//! - diagnostics：结构化诊断记录（code / level / context），有界环形缓冲 +
//!   可注入 sink。
//!
//! 版本化事件 DTO：[`event::ExtensionEvent`] 带 `version` 字段（wire 协议
//! 版本），payload 与产品 `ProductEvent` 完全隔离；字段缺失通过
//! `#[serde(default)]` 向后兼容。
//!
//! 骨架阶段的扩展点：dispatch loop 的 `on_event` 槽位（ARC-710 接
//! runner）、`ExtensionHostOptions`（ARC-710/720 扩展配置字段）、
//! manifest 的 `capabilities` 与 budget 维度（ARC-720 MCP 注册入口）。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// module organization consulted; crate-level design is Evo's own.
pub mod api;
pub mod budget;
pub mod config;
pub mod diagnostic;
pub mod discovery;
pub mod error;
pub mod event;
pub mod host;
pub mod trust;

pub use crate::api::*;
