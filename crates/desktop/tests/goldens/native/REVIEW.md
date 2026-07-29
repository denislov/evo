# Native visual golden review

## VUI-310 review note

Target surface: the Sessions panel in its docked and narrow-overlay forms.

The panel now carries three explicit sections in one bounded vertical scroll region: a full-row New conversation entry, the read-only global skill catalog supplied by the project-independent CAG-103 snapshot, and the existing searchable session history. The former Header plus action has been removed; the Sessions Header, overflow menu, and narrow-overlay close action remain intact. Selecting New conversation preserves the active session workspace and opens Home without dispatching a runtime command or creating session persistence.

- Wide: the 300 px dock keeps the complete New conversation label, one global skill card, and the existing search plus whole-row session interaction. Section labels and divider lines establish hierarchy without widening the panel or moving Conversation / Inspector bounds.
- Medium: the same docked tree remains legible at the constrained workspace width. The optional `Start from Home` detail is omitted in the dock so the primary label never truncates; the skill description wraps inside its bounded card.
- Narrow: the Sessions overlay reuses the same scrollable three-section tree. Its wider row adds `Start from Home`, while the skill card, search field, session metadata, overflow action, and close action all remain inside the modal border.
- Idle wide / medium / narrow: the idle Home surface remains pixel-identical because idle layout intentionally hides side panels. A GPUI regression opens the narrow Sessions overlay from idle Home and proves all three sections, a global skill, search, and a history row are mounted.
- Authorization and reduced motion: modal precedence and motion policy are unchanged; only the underlying Sessions content reflects the new information architecture.
- Keyboard focus and no color: the existing panel focus border, active-session rail, textual section labels, borders, and icons retain usable hierarchy without relying on hue or animation.

Accessibility impact: Sessions remains a named navigation region. Global skills and session history are separately named lists; skills are list items, history retains its named search control and whole-row actions, and New conversation has an explicit accessible description stating that it opens Home without creating a session. The single scroll owner keeps every section reachable in the narrow overlay.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0291628` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0356574` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0665556` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00394308` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0291628` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0291628` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0333702` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
