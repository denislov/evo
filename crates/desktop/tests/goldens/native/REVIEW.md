# Native visual golden review

2026-07-30 完成多项目 Desktop 计划 9.5 最终视觉矩阵审查。

本次保留原有 wide/medium/narrow、idle、authorization、reduced-motion、keyboard-focus
与 no-color 基线，并新增逐张人工审查的 deterministic native fixtures：medium/narrow
Inspector drawer、真实 Provider model popup、真实 capability-driven Thinking popup、
non-reasoning Auto fallback、Home project/long path，以及 catalog loading/error/empty。
Inspector drawer 从 center body 顶部开始且不覆盖 Header；popup 通过 production GPUI input
与 DropdownMenu 路径打开；长路径保持紧凑截断；catalog 和 fallback 均具有非颜色文本语义。

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `medium-inspector` | `1800x1600` | `0` | `medium-inspector-before.png` | `medium-inspector-after.png` | `medium-inspector-diff.png` |
| `narrow-inspector` | `1400x1600` | `0` | `narrow-inspector-before.png` | `narrow-inspector-after.png` | `narrow-inspector-diff.png` |
| `wide-model-menu` | `2600x1656` | `0` | `wide-model-menu-before.png` | `wide-model-menu-after.png` | `wide-model-menu-diff.png` |
| `wide-thinking-menu` | `2600x1656` | `0` | `wide-thinking-menu-before.png` | `wide-thinking-menu-after.png` | `wide-thinking-menu-diff.png` |
| `wide-thinking-non-reasoning` | `2600x1656` | `0` | `wide-thinking-non-reasoning-before.png` | `wide-thinking-non-reasoning-after.png` | `wide-thinking-non-reasoning-diff.png` |
| `wide-home-project` | `2600x1656` | `0` | `wide-home-project-before.png` | `wide-home-project-after.png` | `wide-home-project-diff.png` |
| `wide-home-long-project` | `2600x1656` | `0` | `wide-home-long-project-before.png` | `wide-home-long-project-after.png` | `wide-home-long-project-diff.png` |
| `wide-catalog-loading` | `2600x1656` | `0.00160313` | `wide-catalog-loading-before.png` | `wide-catalog-loading-after.png` | `wide-catalog-loading-diff.png` |
| `wide-catalog-error` | `2600x1656` | `0` | `wide-catalog-error-before.png` | `wide-catalog-error-after.png` | `wide-catalog-error-diff.png` |
| `wide-catalog-empty` | `2600x1656` | `0` | `wide-catalog-empty-before.png` | `wide-catalog-empty-after.png` | `wide-catalog-empty-diff.png` |
| `wide-no-color` | `2600x1656` | `0` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
