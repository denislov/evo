# Native visual golden review

## VUI-103 review note

Target surface: the docked and narrow-overlay Sessions manager.

- Session rows are now full-width, keyboard-focusable action rows backed by a
  real Button. The selected session uses a stable background plus a
  non-colour selection rail; recent sessions no longer carry a permanent
  `Open` button.
- New, overflow, search, clear, and overlay-close actions use the bundled icon
  vocabulary with stable hit targets and accessible labels.
- Search remains owned by SessionsPane and now has visible search/clear
  affordances. Empty, loading, filtered-empty, and omitted states are explicit.
- The narrow Sessions overlay mounts the same SessionsPane entity instead of
  maintaining a second catalog, pending-state, and row renderer. Create,
  refresh, open, search, disabled, and selected semantics therefore cannot
  drift between docked and overlay layouts.

Wide and medium captures were reviewed side by side. The Sessions rail is
quieter and denser without changing its width or the conversation/Composer/
Inspector geometry. The deterministic catalog is empty, so the new empty state
is visible and does not collide with search or the header tools. Narrow base
geometry is unchanged; the dedicated responsive-drawer test verifies the
shared searchable pane, minimum tool targets, Escape handling, scroll
preservation, and focus restoration inside the narrow overlay.

Authorization, reduced-motion, keyboard-focus, and no-color captures preserve
their existing state cues. Session command intent, ledger association, recent
ordering, relative time, refresh cadence, and omitted-count bounds are
unchanged.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0176991` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0216407` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0023853` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0176991` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0176991` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0203384` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
