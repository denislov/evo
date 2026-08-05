# Phase 3 / ARC-340：Merge protocol

> 状态：完成（2026-08-05）
> 前序：ARC-300 `workspace-runtime`、ARC-310 WorktreeBuilder、ARC-320 Registry/恢复/GC、ARC-330 Child capability 隔离
> 目标：child 不再直接写回父目录；一切回写都经过显式、可审计的 merge 操作

## 决策

- **Child terminal 只产生 proposal**：agent/team child 成功结束后 worktree 不再
  立即回收，而是 `Active -> MergePending` 保留，并发布
  `MergeProposalCreated` 事件；失败/取消的 child 仍立即 discard。父 workspace
  在 merge 前逐字节不变。
- **乐观 merge**：`apply_merge` 要求 Git 父 workspace 仍停在 child 的 base revision
  （`git rev-parse HEAD == base_revision`），否则 `StaleParent`；copy-mode 使用
  creation-time baseline 的内容 identity，不依赖 Git HEAD。
  父侧变更与 child 变更的文件集合有交集时 `Conflict`。两类失败都在写任何
  文件之前检测，父目录保持 byte-identical，proposal 保留 `MergePending`
  可重试。
- **ChangeSet**：registry 在 child 创建时保存不可变 creation-time baseline，之后
  以受 cancellation 保护的 filesystem snapshot/hash 比较 child 与 baseline，包含
  dirty tracked、staged、untracked、删除、rename 结果和 symlink；条目上限
  `MAX_CHANGESET_ENTRIES` 超限直接返回 `ChangeSetTooLarge`，不截断、不应用、不转
  `Merged`。
- **copy-mode**：copy/reflink worktree 与 Git worktree 统一使用 creation-time
  baseline，支持 clean apply、conflict、stale、retry 和 discard。
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
- **事务与 crash recovery**：merge 在 apply 前把父 workspace（跳过 `.git`）备份到
  `transactions/<worktree-id>/backup`，fsync `prepared` journal 后才写父目录；
  每次写入均先在同目录临时路径 stage，再安全 rename，取消或失败恢复 backup。
  startup 会回滚 `Prepared`、补齐 `Applied` 的 `MergePending -> Merged` transition，
  并清理 journal；无 journal 的不完整 backup 会安全删除。Merge/Discard 的
  operation recovery contract 同样提供可解析的终态。

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

## 产品入口与验证

```text
cargo test --locked -p workspace-runtime --all-features
79 passed + 3 API contract tests（含 baseline、copy-mode、symlink、untracked 冲突、
fail-closed limit、取消、Prepared/Applied/incomplete transaction recovery）

cargo test --locked -p coding-agent --all-features
167 passed + 2 API contract + module layering tests（含 lease handoff、完整 proposal
DTO、Merge/Discard/List operation contract、cancellation 和 recovery resolution）

cargo test --locked -p cli
106 passed

cargo test --locked -p desktop --all-features
303 passed，5 ignored，dependency boundary 11 passed

bash scripts/gate.sh
architecture gate 必须保持 `execution_debts=0`；workspace 全量门禁在提交前运行。
```

## 产品接线

- CLI 提供 `/proposals`、`/merge <worktree-id>` 和 `/discard <worktree-id>`，复用
  prepared submission/abortable operation 通道并逐文件显示 `+ / ~ / -` 与统计。
- Desktop Inspector 的 Changes 区提供 proposal 列表、刷新、Merge、Discard；
  command admission 绑定当前 idle session，活跃 prompt 时 fail closed。
- Team supervisor 只能通过当前 session 的 `ListMergeProposals` 选择 proposal，
  后续 Merge/Discard 仍走同一 workspace authority 和 operation contract，不能直接
  写父目录。
- ARC-350 继续扩展跨平台、并发和故障注入矩阵，但不再承担 ARC-340 的功能债务。
