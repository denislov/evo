# `code-intelligence`

Evo 的只读代码智能层：tree-sitter symbol graph、增量索引、持久化 cache、LSP lifecycle/diagnostics，以及面向 tool/context injection 的 bounded query service。

公开入口位于 `code_intelligence::api`。索引不得成为文件写入 authority；文件事实与 watcher 由 `change-tracker` / `workspace-runtime` 提供。

第一方依赖：`change-tracker`、`tool-contract`、`tool-runtime`、`workspace-runtime`。

验证：

```bash
cargo test -p code-intelligence --all-targets
cargo clippy -p code-intelligence --all-targets -- -D warnings
```

