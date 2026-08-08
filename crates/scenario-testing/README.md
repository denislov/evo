# `scenario-testing`

Evo 的跨适配器 scripted scenario test-support。它加载版本化 JSON/YAML scenario，驱动 ProductEvent semantic oracle、mock inference SSE 与 deterministic terminal replay。

该 crate 仅用于测试，不承载产品运行时 authority。CLI/Desktop 必须从各自 adapter 内部生成语义终态，再与同一 reviewed fixture 比较。

第一方依赖：`coding-agent`、`tui`。

验证：

```bash
cargo test -p scenario-testing --all-targets
```

