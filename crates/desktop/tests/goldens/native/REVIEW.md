# Native visual golden review

## VUI-104 review note

- Replaced the fixed 176 px Composer text-button column with one continuous
  bordered surface. The one-line content row is compact, while multiline input
  continues to grow upward within the existing eight-line bound.
- Moved the thinking selector from passive StatusBar chrome into the Composer
  toolbar. The running-mode selector remains a distinct adjacent value control.
- Replaced `Send`, `Sending…`, and running-submit text buttons with one stable
  36 px Submit/Busy icon box and retained typed submit paths and tooltips.
- Integrated rejection and authorization state as a full-width inline status
  row inside the Composer surface, so notices do not change the input width.
- Reviewed wide, medium, and narrow captures: the intended shorter Composer
  exposes more bottom-anchored conversation content without changing Pane
  widths. Authorization, reduced-motion, keyboard-focus, and no-color cues
  remain visible and distinct.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.130769` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.155722` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.176628` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0107094` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.130769` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.130783` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.150121` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
