# `extension-host`

Evo 的扩展 authority：hook discovery/config/trust/budget/gate/runner，以及 MCP JSON-RPC transport、lifecycle、OAuth credential recovery 和 meta tools。

公开入口位于 `extension_host::api`。hook 与 MCP stdio 子进程必须通过 `workspace-runtime` sandbox/process primitive；扩展工具进入统一 `tool-runtime`。

第一方依赖：`observability`、`tool-contract`、`tool-runtime`、`workspace-runtime`。

验证：

```bash
cargo test -p extension-host --all-targets
cargo clippy -p extension-host --all-targets -- -D warnings
```

