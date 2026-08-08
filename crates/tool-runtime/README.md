# `tool-runtime`

Evo 的 typed tool registry 与执行 runtime。它负责 schema validation、requirement enforcement、context/cancellation/deadline/progress 注入和动态/typed tool 的单一 dispatch 路径。

公开入口位于 `tool_runtime::api`。产品授权、文件系统 authority 和 UI 展示不属于本 crate。

第一方依赖：`tool-contract`。

验证：

```bash
cargo test -p tool-runtime --all-targets
cargo clippy -p tool-runtime --all-targets -- -D warnings
```

