# Native visual golden review

## VUI-307 review note

Target surface: the Inspector section tab strip in docked wide/medium layouts and the narrow Inspector overlay.

Tabs now size to their complete label content instead of sharing the available width. Each item keeps explicit horizontal design-token padding, remains on one line, and participates in a bounded horizontal scroll strip. Selecting a tab by pointer or Left/Right keyboard navigation moves selection and focus together and scrolls the target by the minimum amount needed to keep it fully visible. The Runtime attention badge remains attached to the Runtime tab inside the scrolling content.

- Wide: content-sized labels and their horizontal padding remain legible; any overflow is contained in the Inspector strip without changing the conversation geometry.
- Medium: the Inspector remains hidden by the responsive layout, so conversation and Sessions geometry are unchanged.
- Narrow: the overlay presents the same single-line, content-sized strip; selected tabs are kept visible without clipping label text.
- Accessibility: `TabList` and `TabPanel` semantics are unchanged. Each tab now exposes an explicit `Tab` role and selected state, only the active tab is in sequential tab order, and Left/Right wraps across all four sections while preserving focus.

Authorization, reduced-motion, keyboard-focus, and no-color behavior are unchanged except for the intended Inspector tab sizing, focus, and scroll treatment.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0215685` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00290727` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0215685` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0215685` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.024275` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
