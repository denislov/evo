# Native visual golden review

VUI-201 establishes a restrained desktop visual language with one public
radius scale, flat information hierarchy, neutral working surfaces, and shared
control geometry:

- Public radii are now 4/6/8 px. Conversation cards, overlays, status chrome,
  the Composer, selectors, and compact semantic markers remain visibly related
  without exceeding the 8 px ceiling.
- Failed Tool and Diagnostic rows use the neutral Tool surface. Danger remains
  explicit through the red rail, ISSUE/failed text, and destructive action
  labels rather than tinting the entire content region.
- Reasoning uses the neutral elevated surface; purple is limited to its rail,
  disclosure label, and state affordance.
- Tool ARGUMENTS uses a left divider and spacing instead of a rounded,
  bordered inner card. The full-message document uses top/bottom dividers, and
  the truncated-preview warning is an attached bottom strip rather than a
  floating nested card.
- Icon buttons, selectors, and Inspector Tabs retain the compact 32 px
  geometry. Header Abort, authorization decisions, Inspector recovery
  decisions, and inline Diagnostic recovery decisions share one fixed 40 px
  critical-action geometry.

All seven deterministic fixtures were reviewed at original resolution. Wide,
medium, and narrow layouts retain readable wrapping, stable transcript tails,
aligned headers, and unobstructed Composer/status regions. The authorization
fixture shows three aligned 40 px decisions with explicit Deny, Allow once,
and Allow for operation labels. Reduced-motion preserves static geometry, and
keyboard focus remains visible without reflow.

The no-color fixture retains non-color hierarchy: the selected Changes Tab has
a filled background and outline, disabled/muted content remains lower
contrast, Diagnostic exposes ISSUE and failed text plus a rail, Reasoning and
Tool remain named disclosure rows, and warning/destructive paths keep explicit
semantic labels. Color therefore reinforces meaning but is not its only
carrier.

Accessibility impact: this token and hierarchy pass removes no roles,
accessible labels, tooltips, tab stops, or typed keyboard paths. Icon tools
remain labelled 32 px controls; Tool and Reasoning retain Button roles and
expanded state; consequential authorization, recovery, and Abort actions
remain explicit text controls with stable 40 px geometry. Focus and no-color
fixtures confirm that state remains perceivable without layout movement or
color-only meaning.

Expanded Tool ARGUMENTS, full-message document dividers, truncated-message
strip, Header Abort, authorization decisions, Inspector Tabs, and all three
Diagnostic recovery actions are additionally covered by source-contract and
real GPUI geometry/click tests because not every transient state appears in
the base visual fixture.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0098058` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0080866` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.00937881` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0593961` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0098058` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0098058` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.00511339` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
