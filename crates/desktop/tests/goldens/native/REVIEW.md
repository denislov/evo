# Native visual golden review

将 Model、Thinking、Profile selectors 从 Header 迁移到 Composer 底栏；Thinking 改为独立 selector；移除 Composer 项目目录 chip；Model 仅显示 model name，Thinking 仅显示 level；保留响应式换行、disabled 状态和 fallback hint。

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0234792` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0237829` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0191619` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0319821` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0846733` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0412863` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00335651` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0234792` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0235523` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `medium-inspector` | `1800x1600` | `0.0262175` | `medium-inspector-before.png` | `medium-inspector-after.png` | `medium-inspector-diff.png` |
| `narrow-inspector` | `1400x1600` | `0.0297644` | `narrow-inspector-before.png` | `narrow-inspector-after.png` | `narrow-inspector-diff.png` |
| `wide-model-menu` | `2600x1656` | `0.0354603` | `wide-model-menu-before.png` | `wide-model-menu-after.png` | `wide-model-menu-diff.png` |
| `wide-thinking-menu` | `2600x1656` | `0.0357993` | `wide-thinking-menu-before.png` | `wide-thinking-menu-after.png` | `wide-thinking-menu-diff.png` |
| `wide-thinking-non-reasoning` | `2600x1656` | `0.0347639` | `wide-thinking-non-reasoning-before.png` | `wide-thinking-non-reasoning-after.png` | `wide-thinking-non-reasoning-diff.png` |
| `wide-home-project` | `2600x1656` | `0.0323076` | `wide-home-project-before.png` | `wide-home-project-after.png` | `wide-home-project-diff.png` |
| `wide-home-long-project` | `2600x1656` | `0.0344934` | `wide-home-long-project-before.png` | `wide-home-long-project-after.png` | `wide-home-long-project-diff.png` |
| `wide-catalog-loading` | `2600x1656` | `0.0319821` | `wide-catalog-loading-before.png` | `wide-catalog-loading-after.png` | `wide-catalog-loading-diff.png` |
| `wide-catalog-error` | `2600x1656` | `0.0319821` | `wide-catalog-error-before.png` | `wide-catalog-error-after.png` | `wide-catalog-error-diff.png` |
| `wide-catalog-empty` | `2600x1656` | `0.0319821` | `wide-catalog-empty-before.png` | `wide-catalog-empty-after.png` | `wide-catalog-empty-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0270069` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
