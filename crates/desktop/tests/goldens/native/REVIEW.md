# Native visual golden review

## VUI-302 status strip removal and transient toast host

- Target surface: the persistent 30 px status strip is removed from the shell,
  and every existing preference/runtime notice path now feeds the bottom-right
  transient ToastHost. The standard deterministic fixture supplies one explicit
  notice alongside the existing disconnected-runtime notice so stacking is
  exercised in wide, medium, and narrow layouts.
- Wide review: Sessions, Conversation, Composer, and Inspector extend cleanly to
  the bottom edge without a residual strip or divider. Both toasts remain inside
  the viewport, preserve full text, and expose distinct close buttons.
- Medium review: the toast width remains bounded while long text wraps without
  clipping. The overlay does not resize the conversation or Composer and the
  reclaimed vertical space remains visible beneath the transcript.
- Narrow review: the Sessions dialog remains the active modal fixture and the
  ToastHost renders above it at the lower-right edge. The two-item stack stays
  within the viewport. Its temporary overlap with the Composer is accepted for
  the six-second transient policy; hover or keyboard focus pauses dismissal and
  every toast can be closed manually.
- Regression scope: the old changed-file summary and command-palette hint are
  absent at all three sizes. Header identity/actions remain non-overlapping, and
  no model, profile, thinking, or focus surface moved into the toast layer.
- Accessibility: each notification uses the status role, retains the complete
  product-authored message as its accessible label, and provides a labelled
  keyboard-focusable dismiss control. Information remains textual rather than
  color-only.

Approved after manual before/after inspection on 2026-07-29 in the pinned X11
native replay environment.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.13682` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.165136` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0501241` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0497505` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0600396` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0669763` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0244523` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.134964` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.135009` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.156819` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
