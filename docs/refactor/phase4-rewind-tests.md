# Phase 4 / ARC-440：Review/rewind 测试矩阵与 full reconcile

> 状态：完成（复验 2026-08-06）
> 前序：ARC-400 `FsEventService`、ARC-410 `HunkTracker actor`、ARC-420 Review domain、ARC-430 Rewind
> 目标：为 WatchGap 增加 full reconcile，并闭合 Phase 4 Gate 的跨域测试矩阵

## 决策

- **full reconcile 重新扫描 tracked 文件**：`ActorState::reconcile` 在
  `ReconcileState::Required` 时逐文件调用 `read_observed` 重新读取磁盘，更新
  `current` 版本，对变化的文件以 `ExternalEditOnAgentFile`（agent 曾触碰）或
  `ExternalEdit`（纯外部）来源调用 `recompute_and_record` 生成新 fact 与 snapshot，
  最后把 `reconcile` 重置为 `Ready`。未变化的文件（revision 匹配 current）跳过，
  不产生冗余 fact。reconcile 是幂等的：部分文件失败时已恢复的文件保持更新
  （current 匹配磁盘，重试时不会变化），失败返回错误并保持 `Required`，消费者可重试。
- **reconcile 保留 baseline 与 fact history**：reconcile 只更新 `current`，不覆盖
  `baseline`（已 accept 的状态）或既有 `facts`（不可变历史）。丢失事件期间的来源
  归因无法恢复，变化统一标记为 external；但旧 Agent fact 仍保留在 history 中，
  不会被外部 reconcile 覆盖。
- **forwarder 自动触发 reconcile**：`HunkTrackingService` 的 forwarder 在 broadcast
  lag 产生 `WatchGap` 后，先 `observe_wait(WatchGap)` 标记 `Required` 并清除 pending，
  再自动调用 `handle.reconcile()`。成功则 `Ready`，消费者立即看到可信状态；失败则
  保持 `Required`，消费者通过 snapshot 可见并决定是否重试。forwarder 不因 reconcile
  失败而退出。
- **显式命令接口**：`CommandKind::Reconcile` + `HunkTrackerHandle::reconcile()` 允许
  消费者在检测到 `Required` 后显式重试，不依赖 forwarder 自动触发。
- **reconcile 后 checkpoint 可用**：`checkpoint()` 在 `Required` 时 fail closed；
  reconcile 成功后 `Ready`，checkpoint 恢复可用。这闭合了 ARC-430 rewind 与
  WatchGap 的交互：WatchGap 后先 reconcile 再 checkpoint，rewind 不会因 `Required`
  被阻塞。

## 落点

| 变更 | 位置 |
| --- | --- |
| `ActorState::reconcile` | `crates/change-tracker/src/hunk/actor.rs` |
| `CommandKind::Reconcile` + `HunkTrackerHandle::reconcile` | `crates/change-tracker/src/hunk.rs` |
| forwarder 自动 reconcile | `crates/change-tracker/src/hunk.rs`（`HunkTrackingService::start`） |
| reconcile 测试 | `crates/change-tracker/src/hunk_tests.rs` |

## 验证

```text
cargo test --locked -p change-tracker --all-features
66 passed（含 6 项 reconcile 测试：restore current、noop when ready、
enables checkpoint、handles deleted、skips unchanged、checkpoint round-trip）

cargo clippy --workspace --all-targets --all-features -- -D warnings
通过

cargo test --locked -p coding-agent --all-features rewind
4 passed（rewind 端到端测试未受 reconcile 改动影响）

bash scripts/architecture-gate.sh
通过（rust_files=632，dependency_edges=17，oversized_debts=35，execution_debts=0）
```

跨域测试矩阵覆盖（ARC-410/420/430/440 合计）：

| 场景 | 覆盖来源 |
| --- | --- |
| Agent 写后外部编辑 | ARC-410 receipt-then-event 因果匹配 |
| 外部编辑后 Agent 写 | ARC-410 event-then-receipt 因果匹配 |
| rename 后继续编辑 | ARC-410 rename chain / atomic path rewrite |
| hunk 漂移 | ARC-410 HunkId 内容 fingerprint + 位置消歧 |
| 相邻 hunk 合并 | ARC-410 best_identity_match |
| 冲突 reject | ARC-420 prepare_reject_hunk/file + stale target |
| stale accept | ARC-420 validate_disk_revision + stale revision |
| watcher event 丢失后 reconcile | ARC-440 full reconcile（本次） |
| rewind 后继续 prompt | ARC-430 durable_rewind 端到端 |
| rewind 后 source branch 导出 | ARC-430 replay_branch + export HTML |
| crash reopen | ARC-430 sidecar missing/corrupt/wrong-owner |
| reconcile 后 checkpoint round-trip | ARC-440（本次） |

## 后续

- Phase 5：rewind 后 prompt queue 的显式同步当前由 `cancel_all` + capability generation
  重置覆盖；ARC-510 prompt queue actor 化后应直接重置 queue 到 checkpoint 时刻。
- Phase 9/10：CLI/Desktop rewind 产品入口与 rewind dispatch runner 提取。
