# `observability`

Evo 的可观测性边界：secrets/path scrub、outbound size budget、telemetry consent/schema 和不含用户内容的 crash report。Telemetry 默认关闭。

公开入口位于 crate root。业务 crate 只产生结构化 lifecycle event，不得自建 exporter、panic hook 或第二套脱敏逻辑。

第一方依赖：无。

验证：

```bash
cargo test -p observability --all-targets
cargo clippy -p observability --all-targets -- -D warnings
```

