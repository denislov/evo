# Phase 4 / ARC-410：HunkTracker actor

> 状态：完成（复验 2026-08-06）
> 前序：ARC-400 `FsEventService`
> 目标：把 mutation receipt 与文件系统事实按 revision 建立因果关系，形成 Review
> domain 可消费的稳定 hunk 与来源事实

## 决策

- **`ChangeReceipt` 归属文件事实域**：类型从通用 `tool-contract` 移入
  `change-tracker`，`coding-agent` 的 write/edit/hashline/apply_patch producer 直接使用该
  类型；不建立 `change-tracker -> tool-contract` 反向依赖，也不保留旧 re-export。
  write 与 apply_patch 现在和 edit/hashline 一样尽可能保存 bounded unified diff；二进制
  或超预算内容保留 revision/delta，不伪造文本 diff。
- **单 actor 所有权**：`HunkTracker` 独占文件状态、pending receipt/event、稳定 ID
  registry、fact ledger 和 reconcile 状态。cloneable `HunkTrackerHandle` 只通过 bounded
  mpsc + oneshot 提交命令和查询 snapshot；queue 满、actor 关闭与非法 fact 都返回
  structured error。
- **端到端组合入口**：`HunkTrackingService` 同时持有 `FsEventService` 与
  `HunkTracker`，用带背压的 forwarder 消费 semantic stream。broadcast lag 被转换成
  `WatchGap`，不允许调用方漏掉 reconcile 语义。ARC-420 在 session runtime 持有该
  service 并投影 product Review API，不再自行搭 event loop。
- **因果匹配不是 path/time 猜测**：receipt 与 fs event 仅在 bounded causal window
  内、workspace-relative path 和实际读取内容的 SHA-256 `after_revision` 同时一致时
  关联；时间只负责关闭窗口。event 先到时暂存实际 observation，receipt 先到时暂存
  expected revision。相同 path 但 revision 不同的 event 在窗口结束后按 external fact
  处理，不会继承 Agent 来源。
- **来源事实完整**：显式 mutation receipt 只接受 `AgentEdit`、`MergeApply`、
  `HookEdit`；无匹配 receipt 的 event 在 Agent 曾触碰的文件上归为
  `ExternalEditOnAgentFile`，否则归为 `ExternalEdit`。每条 immutable fact 保存
  session/turn/operation context（external 为 `None`）、target fingerprint、
  before/after revision、hunks 和记录时间；每路径 latest snapshot 与按 actor 到达顺序的
  fact ledger 同时保留，后续 external edit 不会覆盖早先的 Agent context 证据。
- **稳定 `HunkId`**：unified diff 被解析为独立 hunk；identity 首先按增删内容
  fingerprint 匹配并以新旧位置距离消歧，内容发生局部变化时再按位置重叠匹配。
  因此纯行号漂移保留 ID，同内容的多个 hunk 仍按最近位置一一配对。rename 会原子地
  改写 live file state 与 pending path；目标已有状态时 fail closed，不静默覆盖。
- **bounded / fail-closed**：独立限制 command queue、pending facts、tracked files、
  fact history、每文件 hunk、单 diff bytes、diff lines、累计 history bytes 和 retained
  content bytes。文件 revision 使用流式 SHA-256；超 content budget 后不保留内容、
  不计算 diff。所有 receipt path 必须是安全的相对路径，磁盘 observation 解析后必须
  仍位于 canonical workspace root 内。
- **gap 明确化**：`WatchGap` 清除不再可信的 pending 因果候选并累计
  `ReconcileState::Required { lost }`。本 ARC 不假装自动补全丢失事实；ARC-440 的
  full reconcile 完成前，消费者必须把 snapshot 视为需要重建。

## 落点

| 变更 | 位置 |
| --- | --- |
| receipt 事实类型 | `crates/change-tracker/src/receipt.rs` |
| actor、组合 service | `crates/change-tracker/src/hunk.rs` |
| diff/hunk identity 与 bounded observation | `crates/change-tracker/src/hunk/{diff,observation}.rs` |
| actor/因果/预算测试 | `crates/change-tracker/src/hunk_tests.rs` |
| filesystem producer | `crates/coding-agent/src/tools/filesystem/{mutation_receipt,write,edit,hashline,apply_patch}.rs` |
| 依赖方向 | `scripts/architecture/internal-dependencies.tsv` |

## 验证

```text
cargo test --locked -p change-tracker --all-features
38 passed（含 ARC-400 的 22 项 watcher 测试，以及 receipt/event 双顺序、revision
不匹配、五类 source、turn/session/operation fact ledger、HunkId 位置漂移、rename 后
继续编辑、真实 watcher forward、WatchGap、diff/content/history/queue budgets、snapshot
determinism 与 shutdown）

cargo test --locked -p coding-agent tools::filesystem --all-features
46 passed（含 write/apply_patch bounded ChangeReceipt diff producer）

cargo clippy --locked -p change-tracker --all-targets --all-features -- -D warnings
cargo clippy --locked -p coding-agent --all-targets --all-features -- -D warnings
通过
```

## 后续

- ARC-420：由 session runtime 长期持有 `HunkTrackingService`，filesystem mutation
  成功后直接提交 typed receipt；changed-file/list/open/accept/reject 使用 tracker
  snapshot 构造统一 product DTO，不再 fold ToolOutput JSON。
- ARC-430：持久化/恢复 tracker snapshot，并与 session cursor、workspace snapshot、
  active branch 一致 rewind。
- ARC-440：为 WatchGap 增加 full reconcile，并覆盖 stale accept、冲突 reject 与 rewind
  后继续编辑。
