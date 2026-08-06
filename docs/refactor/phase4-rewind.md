# Phase 4 / ARC-430：Rewind

> 状态：完成（复验 2026-08-06）
> 前序：ARC-400 `FsEventService`、ARC-410 `HunkTracker actor`、ARC-420 Review domain
> 目标：把 rewind 从 tool event 投影升级为 session event cursor + workspace snapshot +
> active branch 三域一致恢复，rewind 是新 branch，不截断历史日志

## 决策

- **三域一致恢复**：rewind 同时恢复 session event cursor（active branch 切换）、
  workspace snapshot（文件内容写回）和 hunk tracker checkpoint（文件事实与来源归因）。
  三域在同一个 admitted durable operation 内提交，任一域失败都触发结构化回滚，
  不允许出现部分恢复的中间态。
- **rewind 是新 branch，不截断历史**：`commit_rewind` 追加一条 `SessionRewound` 事件，
  携带 `source_branch_id`、`new_branch_id`、`checkpoint_id`、`target_leaf_id` 和
  `restored_session_sequence`，并把 manifest 的 `active_branch_id` 切到新 branch。
  `branch_events` 按 branch 父子关系和 `restored_session_sequence` cutoff 过滤事件，
  旧 branch 的事件不被删除，仍可通过 `replay_branch` 或 export 完整导出。cycle、
  duplicate parent、空 branch id、restored sequence 不小于事件 sequence 均 fail closed。
- **workspace restore 是事务**：`restore_workspace_and_tracker` 先 stop tracking，
  再用 `workspace_restore_plan(current, target)` 计算 `WorkspaceRestorePlan`，然后
  `restore_workspace_snapshot` 按 expected/replacement 对逐路径 preflight（内容 revision
  必须匹配 expected）、apply（临时文件 + 安全 rename）、rollback（失败时恢复 prior 条目）。
  workspace restore 失败时尝试 `restore_checkpoint(current)` 恢复 tracker；tracker restore
  失败时回滚 workspace 再恢复 tracker current。两条路径都失败返回 `PartialCommit`。
- **commit_rewind 失败回滚 workspace**：session writer commit 是 rewind 的最后一步。
  若 `commit_rewind` 失败，dispatch 用 `restore_workspace_and_tracker(target, current)`
  把 workspace 回滚到 rewind 前的状态；回滚也失败则返回 `PartialCommit`，不留下
  workspace 已恢复但 branch 未切换的中间态。
- **checkpoint 持久化与验证**：`RewindCheckpoint` 以 JSON 原子写入 session 目录下的
  `rewind/<checkpoint_id>.json`（临时文件 + fsync + 目录 sync + create_new 防覆盖），
  上限 64 MiB。checkpoint 携带 version、session/branch/leaf identity、session_sequence、
  `HunkTrackerCheckpoint`、`WorkspaceSnapshot` 和 SHA-256 digest；`load` 和 `validate`
  重新计算 digest 并校验 session ownership、workspace snapshot bound 与 identity 非空。
  `create_rewind_checkpoint` 在 session 事件提交失败时删除 sidecar，不留下孤儿 checkpoint。
- **startup recovery**：session 打开时 `startup_rewind_checkpoint` 加载上次 rewind 的
  checkpoint 并 `restore_checkpoint` 恢复 tracker；若 workspace 在 shutdown 后被外部
  修改（snapshot 不再匹配 checkpoint），以 `Stale` 拒绝打开，不静默用 checkpoint 覆盖
  外部修改。`cleanup_orphans` 清理 `.tmp-` 临时文件。
- **恢复后同步四个域**：
  1. **hunk tracker**：`restore_checkpoint(target.tracker)` 恢复文件事实、stable HunkId、
     来源归因和 fact ledger；恢复前先 `stop_tracking` 停止 watcher + projection task，
     恢复后 `refresh_latest` 推送新 snapshot。
  2. **capability generation**：`reset_after_rewind(restored_session_sequence)` 递增
     capability generation，使 rewind 前发出的所有 operation permit 失效。
  3. **client cursor**：`cancel_all("tool authorization invalidated by session rewind")`
     取消所有活跃 tool authorization；旧 client connection 的 cursor 因 generation
     不匹配被 `StaleGeneration` 拒绝；新 connection 的 `last_session_sequence` 恢复到
     checkpoint sequence、`last_event_sequence` 重置为 0、drafts 清空。
  4. **session event cursor**：`active_branch_id` 切到新 branch，`active_leaf_id`
     恢复到 checkpoint 的 leaf。
- **fail-closed 边界**：source workspace（非 projectless/managed）拒绝创建 checkpoint
  （`UnsupportedCapability`）；`RewindCheckpointCreated` 事件在 sidecar 写入失败时回滚；
  checkpoint identity 不匹配请求、checkpoint 属于其他 branch、checkpoint sequence 超前
  于当前 cursor 均返回 `Input` 错误；sidecar missing/corrupt/wrong-owner 在 session
  打开时拒绝；tracker `ReconcileState::Required` 时拒绝 checkpoint；snapshot capture
  遇到 symlink 等非普通文件条目（`CapWalkEntryKind::Other`）直接失败，含 symlink 的
  workspace 无法创建 rewind checkpoint（详见"已知问题"）。
- **Operation contract**：`CreateRewindCheckpoint` 和 `Rewind { checkpoint_id }` 是两个
  admitted durable operation（`SessionWriteRoot`），admission 强制 `SessionWriteCapability`，
  仅 persistent Rust-native session 支持；非 persistent session 返回
  `UnsupportedCapability`。`CreateRewindCheckpoint` 通过 `ReviewService::checkpoint()`
  同时捕获 tracker checkpoint 和 workspace snapshot，并用 `validate_tracker_workspace`
  交叉验证 tracker `current` revision 与 workspace snapshot revision 一致后才提交。

## 落点

| 变更 | 位置 |
| --- | --- |
| `WorkspaceSnapshot` / `WorkspaceRestorePlan` / capture / restore | `crates/workspace-runtime/src/rewind.rs`、`api.rs` |
| `HunkTrackerCheckpoint` / checkpoint / restore_checkpoint | `crates/change-tracker/src/hunk/checkpoint.rs`、`hunk.rs` |
| `RewindCheckpoint` 持久化（save/load/remove/cleanup_orphans） | `crates/coding-agent/src/session/rewind.rs` |
| `create_rewind_checkpoint` / `commit_rewind` / `load_rewind_checkpoint` | `crates/coding-agent/src/session/service/commands.rs`、`queries.rs` |
| `branch_events` / `active_branch_events` / `replay_branch` | `crates/coding-agent/src/session/repository/store.rs` |
| `SessionRewound` / `RewindCheckpointCreated` 事件 | `crates/coding-agent/src/session/event.rs`、`replay/fold.rs` |
| `ReviewService::checkpoint` / `restore_checkpoint` / `restore_workspace_and_tracker` | `crates/coding-agent/src/services/review.rs` |
| `CreateRewindCheckpoint` / `Rewind` operation contract + dispatch | `crates/coding-agent/src/application/operation/{contract,dispatch,admission}.rs` |
| `reset_after_rewind` capability 同步 | `crates/coding-agent/src/application/snapshot/capability_state.rs` |
| startup rewind recovery | `crates/coding-agent/src/runtime/facade/lifecycle.rs` |
| manifest `active_branch_id` / `active_leaf_id` | `crates/coding-agent/src/session/manifest.rs` |
| `SessionTransactionWriter` branch_id 支持 | `crates/coding-agent/src/session/transaction.rs` |
| 依赖方向 | `scripts/architecture/internal-dependencies.tsv`（无新边） |

## 验证

```text
cargo test --locked -p change-tracker --all-features
60 passed（含 checkpoint restore、corrupt checkpoint 拒绝）

cargo test --locked -p workspace-runtime --all-features
91 unit tests + 3 API contract passed（含 restore modified/created/deleted、
stale target preflight、source workspace 拒绝、failure rollback、partial write
uncertainty、rollback removes parents）

cargo test --locked -p coding-agent --all-features
182 unit tests + 2 API contract + 2 module layering + 7 doctests passed
（含 durable_rewind_restores_workspace_tracker_branch_and_client_state、
source_workspace_rejects_rewind_checkpoint_before_sidecar_creation、
rewind_branch_tests cycle/duplicate/cutoff 检测）

cargo clippy --locked -p workspace-runtime -p change-tracker -p coding-agent \
  --all-targets --all-features -- -D warnings
通过

bash scripts/architecture-gate.sh
通过（rust_files=632，dependency_edges=17，oversized_debts=35，execution_debts=0）
```

端到端测试 `durable_rewind_restores_workspace_tracker_branch_and_client_state` 覆盖：
checkpoint 创建并验证 sidecar 落盘；checkpoint 后继续编辑（tracked/first-edit/created/deleted）
与 prompt；失败 rewind（损坏 event log）不改变 workspace/tracker/client state；成功 rewind
恢复 workspace 文件、tracker snapshot 与 stable HunkId、切换 branch；旧 connection 因
`StaleGeneration` 拒绝、新 connection cursor 重置（`last_session_sequence` 恢复、drafts 清空）；
rewind 后继续 prompt，active branch 只含 checkpoint prompt + continued prompt，source branch
仍含 excluded prompt 并可 export HTML；crash reopen 覆盖 sidecar missing/corrupt/wrong-owner
拒绝与 orphan cleanup；shutdown 后外部修改 workspace 导致 startup restore 以 `Stale` 拒绝。

架构限制：新增 production Rust 文件均低于 900 行。`embedding.rs`、`operation/contract.rs`、
`operation/dispatch.rs` 因 rewind contract 与 dispatch 内联略有超限，已登记 Phase 10
oversized debt（Phase 10 coding-agent 最终瘦身时提取 rewind dispatch 到 `operations/rewind/runner.rs`
并对齐 merge runner pattern）。

## 已知问题

- **symlink fail-closed**：`capture_workspace_snapshot` 对 `CapWalkEntryKind::Other`
  条目（symlink、FIFO、socket 等）直接报错，含 symlink 的 workspace 无法创建 rewind
  checkpoint。这是保守选择，避免 restore 时越出 workspace 边界或恢复非文件对象；
  但意味着 `node_modules` 等常见含 symlink 的依赖目录会让 checkpoint 不可用。
  后续若要支持，应在 capture/restore 中定义显式 symlink 策略（跟随、拒绝或按
  target 类型存储），并配套测试。
- **flaky 存量测试**：`temp_session_env_repairs_a_partial_commit_on_reopen`
  （ARC-340 ENOSPC 故障注入测试）在并行全量运行时偶发失败，单跑与
  `--test-threads=1` 均通过；与 ARC-430 无关，ARC-440 复测时留意。
- **只读 home 环境注意**：`~/.evo/worktrees` 是默认 worktree registry 路径，
  系统分区只读（如 `/` 挂载为 `ro`）时所有走默认路径的 session 测试会以
  `Read-only file system` 失败；用 `EVO_DIR` 指向可写目录即可全绿。

## 后续

- ARC-440：为 WatchGap 增加 full reconcile（当前 `ReconcileState::Required` 只标记丢失，
  不自动补全），并覆盖 Agent 写后外部编辑 + rewind、rewind 后继续 merge、rename 后 rewind、
  相邻 hunk 合并后 rewind 的跨域矩阵。
- Phase 5：rewind 后 prompt queue 的显式同步当前由 `cancel_all` + capability generation
  重置覆盖；ARC-510 prompt queue actor 化后，rewind 应直接重置 queue 到 checkpoint 时刻的
  owner/version，而非整体取消。
- Phase 9/10：CLI/Desktop rewind 产品入口与 rewind dispatch runner 提取。
