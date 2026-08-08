# `agent-core`

Evo 的 provider-neutral Agent actor 与 turn state machine。它拥有 bounded command queue、prompt queue、context compaction、provider/tool hook 和 transcript 投影，不包含产品会话、文件系统或 UI 策略。

公开入口是 `agent_core::api::{agent,tool,execution,resources,compaction,transcript}`。调用方不得依赖内部 `agent/turn`、`hooks` 或 `context` 模块。

第一方依赖：`ai-protocol`、`tool-contract`、`tool-runtime`。

验证：

```bash
cargo test -p agent-core --all-targets
cargo clippy -p agent-core --all-targets -- -D warnings
```

