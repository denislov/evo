# Native visual golden review

## VUI-304 review note

Target surface: the conversation Header and Composer in active-session and idle/new-task workspaces.

The thinking-level control moves from the Composer footer into a dedicated Header selector beside model and profile. The Header now communicates that thinking is stable session configuration, while the Composer is limited to prompt entry and submission. Active workspaces use compact `M`, `T`, and `P` labels when docked panels reduce chrome width. Idle wide and medium layouts retain the `New task` identity; narrow layouts hide that redundant identity text while keeping the panel toggle, runtime status, all three selectors, inspector toggle, and overflow action fully visible.

- Wide: model, thinking, and profile remain ordered and bounded with both side panels docked; the Composer gains clean space where its thinking override previously lived.
- Medium: all three selectors remain on one line with the Sessions panel docked, and the bottom Composer/toasts remain bounded.
- Narrow: compact selectors remain available without clipping; the Sessions overlay, conversation content, Composer, and toast stack preserve their existing geometry.
- Idle wide/medium/narrow: the same session-default selector is available before the first prompt, and the Composer no longer duplicates the setting.
- Accessibility: the new selector has a stable debug/focus target and the accessible label `Select session thinking level; current …`; every fixed choice exposes checked state. The keyboard-focus fixture keeps a visible focus treatment, and the no-color fixture continues to distinguish state without relying on hue. Hiding the narrow identity text removes only duplicated visible chrome; the task heading remains in the Home surface and the panel toggle retains its accessible label.

The authorization and reduced-motion overlays remain bounded and semantically unchanged. All ten after-captures were reviewed against their before images; the differences are limited to the intended Header/Composer ownership change and responsive chrome accommodation.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0312867` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0306262` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.00467734` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0273317` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0288516` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0824138` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00411609` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0312867` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0313403` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0360673` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
