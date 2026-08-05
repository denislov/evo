# Third-party provenance protocol

任何从 `third-party/` 或外部仓库复制、翻译、重写的实现，都必须在合入生产代码前登记 provenance。

每条记录至少包含：

- upstream repository、commit/SOURCE_REV 和源路径；
- upstream license、notice 路径和派生来源；
- 复制或重写的测试；
- Evo 内的目标 crate/路径；
- 本地语义修改；
- 后续同步策略，或明确声明不跟随上游。

禁止只复制 happy-path 实现而省略安全测试、边界测试或第三方 notice。若一个 Grok 文件又派生自 Codex、
OpenCode 或其他项目，必须同时登记间接来源，不能只写 Grok 的 Apache-2.0。

建议记录格式：

```text
Status: evaluated | adapted | copied | removed
Upstream repository:
Upstream revision:
Source paths:
License/notices:
Destination paths:
Tests carried over:
Local modifications:
Sync policy:
```
