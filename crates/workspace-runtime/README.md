# `workspace-runtime`

Evo 的 workspace authority：opaque access handle、capability-bound filesystem、mutation fence、process tree、background task、sandbox、managed worktree/registry/GC/merge 和 workspace snapshot restore。

公开入口位于 `workspace_runtime::api`。上层不得绕过 handle 直接复制这些 authority，也不得在 sandbox 能力不足时静默降级为 unrestricted。

第一方依赖：无。

验证：

```bash
cargo test -p workspace-runtime --all-targets
cargo clippy -p workspace-runtime --all-targets -- -D warnings
```

