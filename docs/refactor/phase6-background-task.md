# Phase 6 / ARC-600：Background task registry

> 状态：完成
> 前序：Phase 5 Gate（session actor、context compaction、prompt queue）
> 目标：长任务可查询和取消 —— 在 `workspace-runtime` 提供后台进程原语
> （task id / owner / process handle / output spool / terminal state），shell
> 工具支持 foreground/background 双模式，coding-agent 提供产品级
> list / output(cursor) / wait(any/all) / cancel / snapshot 操作与事件。
> Phase 6 Gate 判定项：长任务可查询和取消（本 ARC 覆盖）；
> provider resilience、shell OS policy、web fetch SSRF 由其他 ARC 覆盖。

## 决策

- **spawn 与 run() 共享同一「spawn + attach tree + spool collector」核心**：
  把 `run()` 的 body 提取为 `SpawnedProcess`（spawn / attach 容器 / 取
  pipe，失败时完成 kill+reap 清理）+ `run_until_terminated`（biased
  select：cancellation、可选 timeout、child.wait、双 pipe 读，落点抽象为
  `OutputSink` trait）。`run()` 与 background driver 共用同一读循环与
  终止路径（cancel/timeout 都走 process tree termination + drain grace），
  保证前台与后台的子孙进程清理语义完全一致；`run()` 行为不变
  （同样的错误消息、同样的 tail 渲染、同样的 teardown），既有测试全绿。
  后台任务的超时是显式 `Option<Duration>` 参数（`None` = 无硬超时），
  不偷改 `ProcessSpec.timeout` 的语义。
- **cursor/gap 语义：追加式有界 spool，字节全局偏移作 cursor**：每个进程
  一个 merged stdout+stderr 缓冲，`output(cursor)` 返回 cursor 到缓冲末尾
  的增量字节与 `next_cursor`（= 当前总偏移），读过的字节不重复返回。
  缓冲超过 `output_budget.max_bytes` 时按 UTF-8 边界丢弃最老字节并累计
  `dropped_bytes`；读端 cursor 落后于最老保留字节时，chunk 携带显式
  `OutputGap { dropped_bytes }`，绝不把截断输出伪装成完整输出。最终
  `TaskReport` / `TaskSnapshot` 同样携带 `gap`，产品事件
  （`CodingAgentBackgroundTaskProductEvent::Completed`）带
  `dropped_bytes`。前台 `run()` 的 tail 渲染与 truncation 标注语义不动。
- **owner / shutdown 策略**：`TaskOwner::{Operation, Session, Worktree}`
  轻量标识，`TaskRegistry` 按 owner 分组查询
  （`list_for_owner`）与批量终止（`terminate_all_for_owner`）；
  `TaskRegistry::shutdown` 取消所有运行中任务并 join driver（有界，
  driver 全链路有界：kill + 500ms drain grace），shutdown 后拒绝新 spawn。
  shell 工具的 background 任务 owner 取 `ToolCallContext.operation_id`；
  session 关闭由 `CodingAgentSession::shutdown_internal` 在取消 operation
  之后、提交 terminal 之前调用 `BackgroundTaskService::shutdown()`（registry
  天然 per-session，因为每个 `RuntimeHost` 一个 registry）。
- **产品入口形态：轻量 service 而非 operation**：后台任务不占用
  operation admission / session writer / 取消预算 —— 它属于"工具调用后
  仍然存活的副产物"，用 operation 体系包装只会引入不匹配的 admission、
  durable、child policy 语义。选择 `BackgroundTaskService`
  （持有 `Arc<TaskRegistry>` + `EventService`，挂在 `RuntimeHost`），
  facade 提供 `background_task_*` 方法，与 `CodingAgentSession` 现有
  view/control 方法同构，CLI/Desktop 通过 `coding_agent::api::background`
  + `CodingAgentSession` 消费。
- **shell 参数：`background: Option<bool>`（默认 false）**：`false`/缺省时
  完全走原前台路径（600s 硬超时、progress、tail 渲染），行为与测试不变。
  `true` 时：不设工具级 600s 硬超时；显式 `timeout` 参数变为 task budget
  （任何有限正秒数，不再有 600s 上限），缺省为无硬超时（受
  cancel / owner 终止 / session 关闭兜底）；spawn 后立即返回，tool result
  details 携带 `taskId` / `owner` / `initialCursor`。工具执行的取消 token
  与前台一致来自 `context.cancel` 的 child，但 background 分支不接
  `context.cancel` —— 任务取消走 `TaskHandle::cancel` / owner 终止 /
  session 关闭（前台操作取消不杀后台任务，文档明确）。
- **注入链路：service 经 RuntimeSnapshot 进入工具构建**：operation runner
  （prompt/branch_summary/compaction）各自 `RuntimeService::new()`，因此
  `RuntimeService` 持有的 background service 通过
  `install_background_tasks(&mut RuntimeSnapshot)` 在操作提交时注入
  （`execute_operation_envelope` Async 分支，与 `install_provider_runtime`
  同构；runtime-owned operation 的 `submit_internal` 同样注入），bash 工具
  构建时从 `runtime.background_tasks()` 读取。同步 prompt / 内部操作
  （branch_summary 等）的 bash 工具没有 background service，`background:
  true` 返回 `Unavailable`（fail closed）。

## 落点

| 变更 | 位置 |
| --- | --- |
| spawn/collect/terminate 共享核心 + OutputSink | `crates/workspace-runtime/src/process/mod.rs`（`SpawnedProcess` / `run_until_terminated`，`run()` 重构为组合） |
| TaskId/TaskOwner/TaskState/OutputGap/TaskSnapshot/TaskOutputChunk/TaskReport/TaskHandle/TaskRegistry | `crates/workspace-runtime/src/process/background.rs`（新增） |
| background 测试（完成/增量 cursor/gap/cancel 进程树/wait_all+wait_any/owner 终止/shutdown/超时预算/spawn 失败/owner display） | `crates/workspace-runtime/src/process/background/tests_background.rs`（新增） |
| 公开 facade | `crates/workspace-runtime/src/api.rs` |
| bash 工具 background 模式 | `crates/coding-agent/src/tools/shell.rs` |
| BackgroundTaskService + 公开 DTO | `crates/coding-agent/src/services/background.rs`（新增） |
| service 测试（start/list/cursor/gap/cancel/owner 终止/shutdown/wait 语义/预算/unknown task） | `crates/coding-agent/src/services/background/tests_background_service.rs`（新增） |
| 背景任务事件（Started/Completed/Cancelled/TimedOut/Failed） | `crates/coding-agent/src/events/background_task.rs`（新增）、`events/mod.rs`（`CodingAgentBackgroundTaskProductEvent` + Kind/Family variant）、`events/model.rs`、`services/event/emit.rs` |
| RuntimeService 注入 + bash 接线 | `crates/coding-agent/src/services/runtime.rs` |
| RuntimeHost 持有 service | `crates/coding-agent/src/runtime/owners.rs` |
| 组装 + 注入（from_services/from_transient） | `crates/coding-agent/src/runtime/facade/lifecycle.rs` |
| session 关闭策略 | `crates/coding-agent/src/runtime/facade/connection.rs`（`shutdown_internal`） |
| facade 产品方法（list/output/wait/wait_all/wait_any/cancel/snapshot/terminate_for_owner） | `crates/coding-agent/src/runtime/facade/background.rs`（新增） |
| 公开 API 模块 | `crates/coding-agent/src/lib.rs`（`api::background`、`api::event` 补 variant） |
| 端到端产品测试（prompt 驱动 bash background → facade 查询/等待/取消/关闭终止） | `crates/coding-agent/src/application/operation/background_tests.rs`（新增） |
| 设计文档 | `docs/refactor/phase6-background-task.md`（本文件） |

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
111 passed（ARC-600 新增 12 项 background，见下）
- 后台任务跑完拿全输出（stdout/stderr 分离计数）
- cursor 增量读取且不重放；读空尾部返回同 cursor
- 大输出丢弃最老字节并显式报告 gap（snapshot 与 report 双通道）
- 过期 cursor 读返回显式 gap（不静默丢数据）
- cancel 终止进程树并快速返回 Cancelled；重复 cancel 返回 false
- wait_all / wait_any 跨多任务
- terminate_all_for_owner 只杀指定 owner 的运行中任务
- shutdown 终止运行中任务并拒绝新 spawn
- 显式超时预算 → TimedOut
- spawn 失败（程序不存在）不注册任务
- crash 退出码透传；owner Display 往返

cargo test --locked -p coding-agent --all-features
224 passed（213 lib 单元 + 2 api_contract + 2 module_layering + 7 doc/example；
ARC-600 新增 15 项 = background service 9 + shell 工具 3 + 端到端 3）
- services::background 9 项：start/list/output 增量/gap/cancel/
  owner 终止/shutdown/wait_any+wait_all/预算超时/unknown task
- tools::shell 3 项：background 返回 taskId 且任务继续运行、
  无 service fail closed、显式 timeout 变 task budget（含 >600s）
- application::operation::background_tests 3 项端到端：prompt 驱动
  bash background → facade wait/output 拿全输出；session 关闭终止任务；
  显式 cancel 终止任务
- 其余为事件/注入链路回归

cargo check --workspace --all-features
通过（workspace-runtime / coding-agent / cli / desktop 全编译）

cargo clippy -p workspace-runtime -p coding-agent --all-targets --all-features -- -D warnings
通过（0 warnings）

cargo fmt --all -- --check
通过

scripts/architecture-gate.sh
architecture_gate rust_files=658 dependency_edges=17 oversized_debts=35 execution_debts=0
```

## 后续

- worktree 关闭的 owner 终止接线：`background_task_terminate_for_owner`
  已作为产品 API 提供，managed worktree 生命周期（delegation 的
  merge/discard 路径）后续在 ARC-700 / 相关 ARC 接入
  `TaskOwner::Worktree(...)` 策略。
- ARC-610 sandbox 在 child spawn 边界应用时，background driver 与前台
  `SpawnedProcess::spawn` 共享同一 spawn 入口，沙箱只落一处。
- background 任务当前为内存态（随 session 生命周期）；跨 session 重启
  的任务恢复不在本 ARC 范围。
