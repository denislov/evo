# Native visual golden review

## VUI-306 review note

Target surface: the Sessions list in its docked wide/medium layout and narrow overlay.

Session rows now use the product-owned session name as the primary title. Missing names render as `Untitled`; the stable session id is demoted to the detail line and remains searchable. Each row adds a compact overflow action containing `Rename session`; activating it replaces only that row with a bounded inline name field plus explicit Save and Cancel actions.

- Wide: named rows remain scannable in the docked Sessions panel while the id prefix and two row actions stay bounded.
- Medium: the same name-first hierarchy fits the persisted panel width without changing conversation geometry.
- Narrow: the Sessions overlay has enough width for the full id detail, relative time, rename action, and close action.
- Idle fixtures: no session rows are present, so the new-task surface remains unchanged.
- Accessibility: row labels announce the semantic name, status, and relative update time; unnamed rows announce `Untitled`. Rename, Save, and Cancel actions have explicit labels, and the inline editor receives focus when opened. Search matches both visible name and stable id.

Authorization, reduced-motion, keyboard-focus, and no-color behavior are unchanged except for the intended row metadata/action hierarchy.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.00553907` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.00677263` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0162318` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.000828828` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.00553907` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.00553907` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.00631763` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
