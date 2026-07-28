# Native visual golden review

## VUI-102 review note

Target surfaces: Conversation Header and global StatusBar only.

- Replaced the permanent `Sessions`, `Inspector`, and `...` text buttons with
  bundled panel-direction and overflow icons. Each icon keeps a 32 px stable
  hit target and derives its tooltip/accessibility label from the actual
  show/hide state.
- Replaced the Header's generic configuration button with the one
  model/profile selector. It displays current values plus a dropdown caret;
  its typed model/profile events and disabled rules are unchanged.
- Removed duplicate model/profile controls from the StatusBar. The temporary
  thinking override remains there as the only configuration control until
  VUI-104 moves it into the Composer.
- Kept lifecycle, changed-file count, notice, command hint, and the dangerous
  text-labelled Abort action visible and semantically unchanged.

Wide, medium, and narrow captures were reviewed side by side. The task title,
runtime status, selector, panel toggles, Abort slot, and overflow stay on one
line without overlap or height changes; narrow retains current model/profile
state rather than a generic placeholder. Conversation, Composer, Sessions,
Inspector, and overlay geometry did not move.

Authorization, reduced-motion, keyboard-focus, and no-color captures preserve
their state cues. Icon controls remain discoverable through tooltips,
accessible labels, keyboard focus, and the existing typed-event paths.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0352285` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0398763` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.047071` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0048624` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0352285` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0352285` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0406206` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
