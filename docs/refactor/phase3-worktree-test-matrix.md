# Phase 3 / ARC-350：Worktree 测试矩阵

> 状态：完成（复验 2026-08-05）；跨平台验证债务见 ARC-351
> 前序：ARC-340 Merge protocol
> 目标：把 worktree 生命周期与 merge 协议的故障语义固定为测试事实

## 覆盖矩阵

| 矩阵项 | 测试 | 结果 |
| --- | --- | --- |
| 两个 child 修改不同文件 | `two_children_editing_disjoint_files_merge_sequentially` | 顺序 merge 均 clean，父 workspace 合成两侧变更 |
| 同一文件不同 hunk | `same_file_different_hunks_conflict_on_second_merge` | 第二个 merge `Conflict`，proposal 保持 `MergePending` 可重试 |
| 同一 hunk | `same_file_same_hunk_conflicts_on_second_merge` | 同上 |
| dirty tracked/untracked source | `dirty_tracked_and_untracked_source_are_creation_baseline_state`、`dirty_parent_merge_stays_clean_when_child_paths_are_disjoint`、既有 `parent_untracked_file_conflicts_with_a_child_addition` | 创建前 dirty tracked/untracked 均进入 baseline；创建后的无关 dirty 路径也不受 merge 影响 |
| symlink | `child_added_symlink_is_installed_as_a_symlink`、既有 `replacing_a_parent_symlink_never_writes_through_it`、`git_dirty_symlink_is_preserved`、`copy_fallback_preserves_symlinks` | child 新增 symlink 按 symlink 安装；parent symlink 替换不写穿 |
| rename/delete | `child_rename_applies_as_delete_and_add`、既有 `changeset_lists_added_modified_and_deleted_entries` | rename 表现为 Deleted + Added，merge 后旧路径消失新路径就位 |
| binary | `binary_files_merge_byte_exact` | 含 NUL/非法 UTF-8 的文件字节精确合并，文本行统计为 0 |
| large file | `large_file_changes_merge_without_text_stats` | 8 MiB+ 文件内容正确合并，文本统计按预算边界截为 0 |
| create cancel | 既有 `pre_cancelled_create_removes_destination`、`mid_copy_cancellation_removes_destination`、`cancellation_during_git_worktree_add_kills_the_process_tree_and_cleans_up` | 取消清理物化与 `Creating` 记录 |
| merge crash | `startup_recovery_rolls_back_a_prepared_partial_merge`、`startup_recovery_completes_an_applied_transaction`、`startup_recovery_removes_an_incomplete_transaction_without_a_journal` | drop/reopen registry 后，Prepared 回滚、Applied 补齐 transition、无 journal 清理 |
| disk full / partial apply | `enospc_after_partial_apply_rolls_back_parent_and_keeps_proposal`、`enospc_while_marking_applied_rolls_back_parent_and_keeps_proposal` | one-shot ENOSPC 分别注入第二个 entry 前和 Applied journal 写入点；父 workspace 回滚、journal 清理、proposal 可重试 |
| GC crash | `gc_crash_midway_is_retried_and_converges_on_the_next_pass` | 精确模拟 materialization 已删除、`Ready` record 未删除，再 reopen + GC 收敛 |
| process crash（owner 死亡） | `startup_maintenance_collects_dead_process_records_but_keeps_live_records` | 使用真实已退出子进程 PID，reopen 后 dead-owner 收集、live-owner 保留 |

## 决策

- **文件级冲突是 ARC-350 的固定语义**：两个 child 修改同一文件（无论 hunk 是否
  重叠）在第二个 merge 时按文件内容 identity 报 `Conflict`。hunk 级合并属于
  Phase 4 `change-tracker`（ARC-410 HunkTracker）的收敛范围，本阶段不实现。
- **连续 merge 依赖 merge 不 commit**：merge 只写父工作树不动 HEAD，因此同 base
  的第二个 child 通过乐观 revision 校验；跨文件合成由
  `parent_conflicts` 的内容级比较保证，父工作树被第一个 merge 改动过的路径
  才会冲突。
- **ENOSPC 注入**：沿用 session persistence 的 one-shot fault-plan 思路，只在
  `workspace-runtime` 测试编译中启用。partial-apply 用例保证一个 entry 已安装后才
  让第二个 entry 返回 ENOSPC；Applied journal 用例固定了 journal 持久化失败也必须
  同步 rollback 的生产语义。测试不依赖 Unix mode bit 或 runner 用户权限。
- **GC 崩溃收敛**：直接执行 materialization 清理但保留 durable record，随后 drop
  并 reopen registry，精确覆盖 GC 删除物化与删除 record 之间的崩溃窗口。孤儿目录
  （无记录）仍只报告不删除。
- **crash-reopen**：merge transaction、dead-owner maintenance 和 GC 均重新构造
  `WorktreeRegistry` 后恢复；不再用同一个内存对象代替启动恢复。

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
91 passed + 3 API contract tests（ARC-350 共新增 12 个矩阵用例）

cargo test --locked --workspace --all-features
全部通过

bash scripts/gate.sh
architecture gate 必须保持 execution_debts=0；ARC-351 是验证债务，不是生产代码迁移债务
```

## 后续

- Phase 3 Gate：并行 child 默认完全隔离；父 workspace 在 merge 前逐字节不变；
  异常退出后 registry/worktree 可恢复或安全清理。矩阵补齐后具备 Gate 证据。
- ARC-351：Windows/macOS 真实平台复验是独立非阻塞验证债务，最迟在 Phase 10
  Final Gate 前清偿；CI 资源决策与退出条件以主计划 ARC-351 为准。
- Phase 4 `change-tracker` 引入 hunk 级 review 后，重审"同一文件不同 hunk"
  场景的冲突策略。
