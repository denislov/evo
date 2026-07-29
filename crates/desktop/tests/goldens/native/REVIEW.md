# Native visual golden review

## VUI-309 review note

Target surface: the attention-only runtime indicator in the Conversation Header.

The Header itself and every model, Thinking, Profile, panel, Abort, and overflow control remain intact. `Idle` no longer mounts a glyph, label, background, accessibility node, or duplicate session-state message in the Header. The Sessions row status point remains the quiet session-level carrier, while the Header indicator appears only for Running, Authorization, Warning, and Error.

The indicator owns a viewport-dependent but state-independent horizontal slot: 104 px at medium/wide widths and 80 px below 900 px. Header action spacing similarly uses 8 px normally and 4 px in the narrow layout. An idle slot is visually empty; attention states mount inside exactly the same slot, so status appearance cannot move the identity region, model/Thinking/Profile selectors, panel toggle, or overflow action. Authorization uses `Approval` visually, shortened to `Auth` in the narrow layout, while its `Role::Status` accessible name remains the complete `Authorization required`.

- Wide: the former `○ Idle` pill is absent; the remaining Header controls retain one balanced row, and Conversation/Sessions/Inspector geometry is unchanged.
- Medium: the idle indicator is absent and the compact `M` / `T` / `P` selectors remain fully visible without overlap or clipping.
- Narrow: the 80 px reservation and 4 px action gaps keep every Header control inside the 700 px viewport; the Sessions overlay and underlying Conversation geometry remain unchanged.
- Idle wide/medium/narrow: Home keeps the complete `New task` Header and selectors, with no redundant idle label; recent sessions, global skills, and Composer geometry are unchanged.
- Authorization: `? Approval` remains visible in the reserved slot behind the modal and the complete authorization wording remains available to assistive technology.
- Reduced motion, keyboard focus, and no color: only the intended idle indicator disappears; focus, grayscale hierarchy, selection rails, and control geometry remain legible and unchanged.

Accessibility impact: idle contributes no inert status node. Every non-idle indicator is a `Role::Status` element with the full semantic label. A three-viewport GPUI regression transitions the same Header through Idle, Running, Authorization, Warning, and Error, proves every attention indicator fits its slot, and compares all other Header bounds for exact equality.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.00439503` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0354302` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.00508161` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.00439503` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.00537381` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0378051` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00120577` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.00439503` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.00439503` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.00497643` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
