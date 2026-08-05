# Phase 3 Registry、恢复与 GC 完成记录

日期：2026-08-05

范围：ARC-320 / `workspace-runtime` 持久化 managed worktree registry、启动恢复与 GC。

## 本阶段已完成

### WorktreeRegistry（原子文件 registry）

- 目录布局：`<root>/registry/<id>.json`（每 worktree 一条记录）+ `<root>/worktrees/<id>/`（worktree 本体）。
- `WorktreeRecord`：id、kind、source、dest、owner operation、owner process id、parent session、base revision、creation mode、lifecycle、created/updated unix 秒；serde round-trip。
- 记录原子写：唯一 tmp 文件 + fsync + 跨平台 replace；父目录在 Unix 上同步；残留 `.tmp.<pid>.<sequence>` 文件不参与加载；损坏 JSON fail-closed（`InvalidRecord`）。
- registry writer 使用跨线程/跨进程 file lock 串行化 read-modify-write；所有 public id 入口先解析为 bounded `WorkspaceId`。
- `register/load/recovery/GC` 强校验：id、kind、record filename、dest 直接子目录和 canonical containment 必须一致；中间 symlink、`..` 和非 canonical source 均 fail-closed。
- lifecycle 持久化复用 `valid_lifecycle_transition` 单一转换表（与 `WorkspaceLease` 共享，持久化状态不可能超出内存状态机允许的范围）。
- `create_managed` 高层入口：由 source + owner + 纳秒时间、进程 id 和序列生成唯一 id（`child-<hex>`），先持久化 `Creating`，再调 `WorktreeBuilder`（显式 `worktree_id` 钉住 handle id == 目录名 == 记录 id），materialize 成功后原子写入 `Ready`；进程崩溃留下的失败窗口由恢复处理，正常 cancellation 在返回前移除 `Creating` record。

### identity 对齐

- `WorkspaceHandle::with_explicit_id`：接受完整 id 但校验 kind 前缀，杜绝伪造 kind 的 id；`WorktreeBuilder::worktree_id` 钉住 identity，使 handle id、目录名与记录 id 三者在 registry 场景保持一致（此前 derived id 与目录名必然不一致）。

### 启动恢复（recover）

- `scan` 产出三类不一致：interrupted（Creating/Cleaning 记录）、stale（记录存在但目录缺失）、orphan（目录存在但无记录）。
- `recover`：清理 interrupted 和 stale（验证身份后删除 git 注册、目录、prune、复查 registration，再删除 record）；Git cleanup 无法证明完成时保留 record 并返回错误；**orphan 只报告绝不自动删除**——无记录即无法验证身份。
- 返回恢复前发现的完整报告，供审计。
- `startup_maintenance` 在 registry 打开时自动执行 `recover`，随后依据 owner process liveness 清理已退出进程留下的 `Ready/Active/Merged/Discarded` records；当前 session 的 live records 会保留。

### GC

- `GcOptions`：now（可注入）、max_age_seconds、disk_budget_bytes、owner_liveness predicate、dry_run。
- 候选 = lifecycle 允许 GC、owner 不存活 且 超过 max age 的记录，按 updated_at 最老优先；`Creating`、`MergePending`、`Cleaning` 等非安全收集状态永不成为候选；disk budget 达标即停。
- 删除前 `validate_record` 复核身份（canonical dest 在 worktrees 根下、目录名 == id、id bounded），并以 `symlink_metadata` 统计大小（不跟随 child symlink）；然后 git worktree remove → 目录删除 → 记录删除 → git prune → registration 复查；git-linked 与 copy 两种 creation mode 都覆盖。
- dry-run 返回候选列表不动任何目录。
- `discard` 持久化 `Ready/Active/MergePending -> Discarded -> Cleaning -> Removed`；清理中断后下一次 `recover` 可继续处理，已 `Removed` 或不存在的 id 幂等返回。

## 验证

已通过：

```text
cargo test --locked -p workspace-runtime --all-features
65 passed, 0 failed（含 registry、恢复、startup maintenance、GC 和安全边界测试）

cargo test --workspace --all-features
全部通过

cargo clippy --workspace --all-targets --all-features -- -D warnings
0 errors

cargo fmt --all -- --check
干净

bash scripts/gate.sh
architecture_gate rust_files=602 dependency_edges=15 oversized_debts=32 execution_debts=0 mode=incremental
```

覆盖行为：register/load round-trip、id 唯一性、非法 dest 拒绝、lifecycle 持久化与非法转换拒绝、损坏记录 fail-closed、tmp 残留容错、recover（interrupted/stale 清理 + orphan 保留）、GC（owner liveness、age、disk budget、dry-run）、git-linked GC 连同 git registration 注销、explicit id kind 前缀校验。

## 后续范围

- ARC-330 child capability 隔离：delegation/team 调用 `registry.create_managed` 申请 managed worktree，`WorkspaceAccessHandle` 指向 child worktree；operation 结束时通过 discard 持久化清理，owner process liveness 由 startup maintenance/GC 判断。
- ARC-340 merge protocol：child 产生 `ChangeSet`/`MergeProposal`，parent base revision + child base revision 乐观校验。
- ARC-350 worktree 测试矩阵：并行 child 修改同名文件/hunk、crash（进程 kill 后 recover）、disk full、Windows 路径。
- SQLite 只在这些文件 registry 出现并发查询瓶颈后才引入（计划 §5 的条件移植项）。
