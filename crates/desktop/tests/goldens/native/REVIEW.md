# Native visual golden review

Phase 3 shell golden review, 2026-07-27

Reviewed all seven release fixtures at their pinned viewport sizes. The accepted candidate:

- replaces replay-invisible icon-only controls with visible compact labels;
- keeps the wide, medium, and narrow headers and status bars free of overlap;
- keeps changed-file controls within the Inspector rail;
- preserves authorization, reduced-motion, keyboard-focus, and no-color states;
- preserves conversation, Composer, tool-row, and drawer geometry.

The earlier candidate was rejected before installation because icon-only controls rendered blank and changed-file labels overflowed the Inspector rail.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0591199` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0535913` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0529001` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0126029` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0591773` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0591338` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.067993` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
