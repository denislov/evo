# Native visual golden review

## DSK-503 idle home surface

- Target surface: the native Desktop window while no coding-agent session is open.
- Control semantics: the full-width Home pane now presents the selected model and thinking level,
  a bounded recent-session directory, and user-global skills. The existing Composer is reused as
  the primary input and submission control; submitting remains the point at which a session is
  created.
- Wide / medium / narrow review: all three idle fixtures keep the Home content and Composer inside
  the viewport, hide both session-only side panels, retain readable two-column discovery content,
  and avoid clipping or overlap with the status bar.
- Accessibility: Home is exposed as a named main region, recent sessions remain typed action rows,
  and the idle focus ring contains only Composer and Status. The established-session focus order is
  unchanged.


# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.127544` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.143314` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.159407` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
