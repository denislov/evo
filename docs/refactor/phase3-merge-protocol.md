# Phase 3 / ARC-340：Merge protocol

> 状态：完成（2026-08-05）
> 前序：ARC-300 `workspace-runtime`、ARC-310 WorktreeBuilder、ARC-320 Registry/恢复/GC、ARC-330 Child capability 隔离
> 目标：child 不再直接写回父目录；一切回写都经过显式、可审计的 merge 操作

## 决策

- **Child terminal 只产生 proposal**：agent/team child 成功结束后 worktree 不再
  立即回收，而是 `Active -> MergePending` 保留，并发布
  `MergeProposalCreated` 事件；失败/取消的 child 仍立即 discard。父 workspace
  在 merge 前逐字节不变。
- **乐观 merge**：`apply_merge` 要求父 workspace 仍停在 child 的 base revision
  （`git rev-parse HEAD == base_revision`），否则 `StaleParent`；
  父侧变更与 child 变更的文件集合有交集时 `Conflict`。两类失败都在写任何
  文件之前检测，父目录保持 byte-identical，proposal 保留 `MergePending`
  可重试。
- **ChangeSet**：`git diff --name-status/--numstat base` + `ls-files --others`
  采集 child 相对 base 的变更（含 untracked 新文件），条目上限
  `MAX_CHANGESET_ENTRIES`，超限截断并报告。
- **copy-mode 拒绝**：copy worktree 没有 base snapshot，merge 显式拒绝
  （`CopyWorktreeUnsupported`），执行债务：Phase 4 `change-tracker` 提供
  base snapshot 后收敛。
- **Merge/Discard 是 admitted operation**：`MergeChildWorktree` /
  `DiscardChildWorktree` 为 root operation（SessionWriteRoot + Async），
  admission 时强制 workspace capability（不再依赖工具集推断）；runner 校验
  `record.source == 当前 session workspace root`，跨 workspace 的 worktree
  无法被本 session merge/discard。
- **merge 成功后自动清理**：apply 成功后记录转 `Merged` 并 discard
  （`Merged -> Cleaning -> Removed`，复用 ARC-320 持久化清理路径）；
  apply 失败（conflict/stale）不触碰记录与父目录。
- **事件**：`MergeProposalCreated / Applied / Conflicted / StaleParent /
  Discarded / Failed` 进入 product event stream（新 family `Merge`）。
  冲突/过期以结构化错误 `CodingSessionError::Conflict / Stale` 上抛，
  CLI/desktop 可据此提供 retry 路径。
- **crash recovery**：merge 中断在 apply 与 transition 之间时，记录停在
  `MergePending`，父目录可能部分应用 —— 执行债务：Phase 4 change-tracker
  事务性 review 收敛（ARC-340 的 apply 是逐文件 copy，尚无统一事务边界）。

## 落点

| 变更 | 位置 |
| --- | --- |
| ChangeSet / build_changeset / apply_merge | `workspace-runtime/src/worktree/merge.rs` |
| merge 事件（内部 + 公开 DTO） | `coding-agent/src/events/merge.rs`、`events/mod.rs`、`events/model.rs` |
| Merge/Discard operation（contract/dispatch/outcome） | `coding-agent/src/application/operation/*` |
| merge runner + 授权校验 | `coding-agent/src/operations/merge/runner.rs` |
| child 成功后 promote（agent/team runner） | `operations/agent_invocation/runner.rs`、`operations/team_invocation/runner.rs` |
| admission 强制 workspace | `coding-agent/src/application/capability.rs` |
| `CodingSessionError::Conflict / Stale` | `coding-agent/src/kernel/error.rs` |

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
71 passed（新增 merge 6 个：changeset 采集、clean apply、conflict、stale、
copy 拒绝、非 MergePending 拒绝）

cargo test --locked -p coding-agent --all-features
166 passed（新增 worktree promote 语义、失败释放、merge operation 5 个、
session dispatch e2e 2 个）

cargo test --locked --workspace --all-features
1015 passed

bash scripts/gate.sh
architecture_gate rust_files=608 dependency_edges=15 oversized_debts=32 execution_debts=0 mode=incremental
```

## 后续

- CLI/desktop 接线：`CodingAgentOperation::MergeChildWorktree /
  DiscardChildWorktree` 已通过 `prepare_client_submission` 通道可用，
  交互入口与 merge 工具（Team supervisor 选择 proposal）留待 ARC-350/Phase 4
  产品层。
- ARC-350 worktree 测试矩阵补齐 merge crash / 并发 merge 场景。
