# `event-journal`

Evo 的通用 append-only journal primitive。它拥有有界 frame codec、write lease、torn-tail repair、sequence、checkpoint append 与 bounded tail read，不了解产品事件语义。

公开入口位于 `event_journal::api::{error,frame,read,storage}`。

第一方依赖：无。

验证：

```bash
cargo test -p event-journal --all-targets
cargo clippy -p event-journal --all-targets -- -D warnings
```

