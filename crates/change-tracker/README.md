# `change-tracker`

Evo 的文件事实与 review domain。它把 watcher 事件归一化为 semantic event，维护 baseline→current hunk identity、来源归因、checkpoint、accept/reject plan 和 WatchGap reconcile。

公开入口位于 crate root 的稳定 re-export；文件访问 authority 始终来自 `workspace-runtime`。

第一方依赖：`workspace-runtime`。

验证：

```bash
cargo test -p change-tracker --all-targets
cargo clippy -p change-tracker --all-targets -- -D warnings
```

