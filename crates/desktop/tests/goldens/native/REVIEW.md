# Native visual golden review

## DSK-504 scratch workspace identity

- Target surface: the idle Home summary row for projectless Desktop startup.
- Control semantics: the row now names the active location as `Scratch workspace`
  and shows its bounded path, so users do not mistake agent-created files for
  writes to the process directory or to a selected project. Model and thinking
  facts retain their existing order and meaning.
- Wide / medium / narrow review: the additional metadata remains on the same
  bounded summary row at all three viewports. It does not overlap the Home
  heading, recent sessions, global skills, Composer, or status bar. The
  established-session fixtures are pixel-identical.
- Accessibility: the label is textual rather than color-only, stays inside the
  existing named Home main region, introduces no focus target, and does not
  change the established-session or idle focus order.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0148227` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0181238` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0205504` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
