# `tui`

Evo 的可复用终端 UI primitive：terminal capability/image protocol、输入解析、editor、layout/focus/overlay、render surface、Markdown checkpoint 和 VirtualTerminal test support。

公开入口位于 `tui::api`。本 crate 不依赖产品运行时，产品状态由 CLI adapter 转换成组件输入。

第一方依赖：无。

验证：

```bash
cargo test -p tui --all-targets --features test-support
cargo clippy -p tui --all-targets --features test-support -- -D warnings
```

