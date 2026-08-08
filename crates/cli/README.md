# `cli`

Evo 的命令行产品适配器，生成 `coding-agent` 二进制。它拥有参数解析、TUI/inline/print/JSON/RPC adapter、终端输入输出、update 命令和启动时非阻塞更新提示。

CLI 只能通过 `coding_agent::api` 访问产品运行时；该约束由 Architecture Gate 强制。

第一方依赖：`coding-agent`、`observability`、`release-updater`、`tui`。

验证：

```bash
cargo test -p cli --all-targets
cargo clippy -p cli --all-targets -- -D warnings
```

