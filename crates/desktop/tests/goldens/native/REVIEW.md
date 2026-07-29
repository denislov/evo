# Native visual golden review

## DSK-513 per-session status and workspace limit

- Target surface: each catalog row now reflects the owning workspace's semantic
  runtime state and exposes a trailing close action. The deterministic replay
  supplies one current session so the row is exercised instead of falling back
  to the empty catalog message.
- Docked wide / medium review: the compact panel prioritizes the session name,
  reserves a stable 36 px trailing region, and keeps the close glyph clear of
  the status dot and ellipsized title. No conversation, Inspector, Composer, or
  status-bar geometry changes.
- Narrow review: the replay opens the Sessions dialog so this viewport covers
  the same feature. Its additional width shows cwd/status metadata and relative
  time; the close action remains a distinct fixed-width sibling of the row's
  open action, with no overlap or reflow.
- State variants: authorization, reduced-motion, keyboard-focus, and no-color
  captures preserve their prior surfaces while adding the same bounded row.
  Idle Home captures remain pixel-identical because no runtime session exists.
- Accessibility: every status has a glyph plus a textual label in the row's
  accessible name; the close icon has a session-specific label and is an
  independent control. Selection retains the non-color rail, and the no-color
  fixture remains understandable without semantic hues.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0140014` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0171196` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.134385` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00192509` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0140014` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0140014` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0158261` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
