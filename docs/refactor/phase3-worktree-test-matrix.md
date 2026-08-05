# Phase 3 / ARC-350：Worktree 测试矩阵

> 状态：完成（2026-08-05）
> 前序：ARC-340 Merge protocol
> 目标：把 worktree 生命周期与 merge 协议的故障语义固定为测试事实

## 覆盖矩阵

| 矩阵项 | 测试 | 结果 |
| --- | --- | --- |
| 两个 child 修改不同文件 | `two_children_editing_disjoint_files_merge_sequentially` | 顺序 merge 均 clean，父 workspace 合成两侧变更 |
| 同一文件不同 hunk | `same_file_different_hunks_conflict_on_second_merge` | 第二个 merge `Conflict`，proposal 保持 `MergePending` 可重试 |
| 同一 hunk | `same_file_same_hunk_conflicts_on_second_merge` | 同上 |
| dirty tracked/untracked source | `dirty_parent_merge_stays_clean_when_child_paths_are_disjoint`、既有 `dirty_source_state_is_part_of_the_baseline_not_a_child_change`、`parent_untracked_file_conflicts_with_a_child_addition` | dirty 状态进入 baseline；child 未触碰的 dirty 路径不受 merge 影响 |
| symlink | `child_added_symlink_is_installed_as_a_symlink`、既有 `replacing_a_parent_symlink_never_writes_through_it`、`git_dirty_symlink_is_preserved`、`copy_fallback_preserves_symlinks` | child 新增 symlink 按 symlink 安装；parent symlink 替换不写穿 |
| rename/delete | `child_rename_applies_as_delete_and_add`、既有 `changeset_lists_added_modified_and_deleted_entries` | rename 表现为 Deleted + Added，merge 后旧路径消失新路径就位 |
| binary | `binary_files_merge_byte_exact` | 含 NUL/非法 UTF-8 的文件字节精确合并，文本行统计为 0 |
| large file | `large_file_changes_merge_without_text_stats` | 8 MiB+ 文件内容正确合并，文本统计按预算边界截为 0 |
| create cancel | 既有 `pre_cancelled_create_removes_destination`、`mid_copy_cancellation_removes_destination`、`cancellation_during_git_worktree_add_kills_the_process_tree_and_cleans_up` | 取消清理物化与 `Creating` 记录 |
| merge crash | 既有 `startup_recovery_rolls_back_a_prepared_partial_merge`、`startup_recovery_completes_an_applied_transaction`、`startup_recovery_removes_an_incomplete_transaction_without_a_journal` | Prepared 回滚、Applied 补齐 transition、无 journal 清理 |
| apply 失败（disk full 等价） | `apply_failure_rolls_back_parent_and_keeps_proposal` | stage 写失败时父 workspace 字节一致回滚、事务 journal 清理、proposal 保持可重试 |
| GC crash | `gc_crash_midway_is_retried_and_converges_on_the_next_pass` | 半删 dest + `Ready` 记录由下一次 GC 收敛，不泄漏 |
| process crash（owner 死亡） | 既有 `startup_maintenance_collects_dead_process_records_but_keeps_live_records` | dead-owner 收集，live-owner 保留 |

## 决策

- **文件级冲突是 ARC-350 的固定语义**：两个 child 修改同一文件（无论 hunk 是否
  重叠）在第二个 merge 时按文件内容 identity 报 `Conflict`。hunk 级合并属于
  Phase 4 `change-tracker`（ARC-410 HunkTracker）的收敛范围，本阶段不实现。
- **连续 merge 依赖 merge 不 commit**：merge 只写父工作树不动 HEAD，因此同 base
  的第二个 child 通过乐观 revision 校验；跨文件合成由
  `parent_conflicts` 的内容级比较保证，父工作树被第一个 merge 改动过的路径
  才会冲突。
- **apply 失败注入**：以"目标目录不可写"模拟磁盘满/写失败（stage 步骤失败），
  目录保持为空以允许 rollback 删除与恢复；注入点选在 apply 阶段而非
  changeset 构建阶段，保证端到端走完整事务路径。
- **GC 崩溃收敛**：GC 在删除物化与删除记录之间崩溃时，`dest` 半删、记录仍为
  `Ready`；该状态既不属于 `interrupted` 也不属于 `stale`，由下一次 GC 以同一
  策略重试删除并收敛。孤儿目录（无记录）仍只报告不删除。
- **跨平台推迟**：Unix/Windows 路径差异与 Git worktree 平台差异不在本地
  交叉编译 target 上验证，推迟到真实平台测试（`cfg(unix)` 用例已在本地
  Linux 跑通；Windows 侧的行为由同一组矩阵在 CI/真实机器上复验）。

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
89 passed + 3 API contract tests（新增 10 个矩阵用例）

cargo test --locked --workspace --all-features
全部通过

bash scripts/gate.sh
architecture gate 必须保持 execution_debts=0
```

## 后续

- Phase 3 Gate：并行 child 默认完全隔离；父 workspace 在 merge 前逐字节不变；
  异常退出后 registry/worktree 可恢复或安全清理。矩阵补齐后具备 Gate 证据。
- 真实平台测试：Windows/macOS 上复跑本矩阵（含路径、symlink、文件锁差异），
  发现的行为差异以 fix commit 记录在对应平台。
- Phase 4 `change-tracker` 引入 hunk 级 review 后，重审"同一文件不同 hunk"
  场景的冲突策略。
