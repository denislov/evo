# Native visual golden review

## VUI-303 explicit model and Profile dropdowns

- Target surface: the conversation header replaces the combined cyclic
  `Next model` / `Next profile` menu with two independent DropdownMenu
  controls. Each control names its current value, opens the complete
  project-snapshot catalog, checks the selected item, and submits the exact
  model or Profile id. Unsupported model entries remain visible but disabled;
  Team Profiles are also visible but disabled because the product command sets
  only the default agent Profile.
- Wide review: both full labels (`Model` and `Profile`) fit between the stable
  task identity/status region and the existing panel/action controls. Long
  values truncate within their own selector; no header control overlaps or
  changes the transcript, Inspector, or Composer geometry.
- Medium review: the selectors use the existing compact `M` / `P` labels and
  keep distinct click targets. Current-value text and chevrons remain visible,
  while the existing side-panel reduction leaves the conversation width
  unchanged.
- Narrow review: the compact selectors, status slot, task identity, panel
  toggle, and overflow action all remain on one line. The existing Sessions
  dialog fixture remains modal and bounded; the header does not intersect it.
- Idle review: wide, medium, and narrow Home fixtures now expose the same
  selectors before a session exists. The header is labelled `New task`, uses
  the cwd-free project model/Profile catalog, and stays separate from the Home
  summary and bottom Composer. This intentional new header accounts for the
  larger idle-fixture diffs.
- Long-list behavior: both menus reuse the existing PopupMenu/DropdownMenu
  primitive and become vertically scrollable above eight entries with a
  bounded 320 px menu height. No second menu implementation or cyclic fallback
  remains.
- Accessibility: each selector has a full accessible label including the
  current value, preserves the existing keyboard menu navigation, exposes the
  checked item, and leaves unsupported models disabled instead of allowing an
  invalid selection. Selection is textual and does not depend on color.

Approved after manual before/after inspection of all ten deterministic native
fixtures on 2026-07-29 in the pinned X11 native replay environment.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0245363` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0251692` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.00382332` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0749222` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0901283` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.101382` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00413911` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0245363` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0245363` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0283238` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
