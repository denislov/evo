# Native visual golden review

DSK-690 removes legacy-path and Rust source-text assertion debt without changing
the production Render tree. The ten deterministic captures were replayed and
reviewed; every fixture remains at normalized RMSE `0`, so no golden image is
replaced.

- Wide, medium, narrow, three idle variants, authorization, reduced-motion,
  keyboard-focus, and no-color retain their reviewed geometry and semantics.
- The old Home sections, catalog refresh timer, root-level narrow Inspector
  overlay, and fixed Thinking enumeration are now guarded by typed behavior,
  module-graph/authority tests, and the final deletion audit—not by searching
  implementation source strings.
- Keyboard shortcuts and AccessKit role/focus contracts are documented in
  `docs/architecture.md`; GPUI interaction tests remain the authority for hit
  targets, popup/drawer focus restoration, and non-color selection semantics.

VUI-422 closes the Inspector dock/drawer interaction contract without changing
the reviewed shell pixels. All ten deterministic captures compare at normalized
RMSE `0`, so this task does not replace any golden image.

- Wide keeps the independent docked Inspector header and resize boundary;
  medium and narrow keep the drawer host below the center Header.
- Dedicated GPUI hit tests click the Header Profile selector while the Inspector
  drawer is open at `1000x900` and `700x900`, select the exact project profile,
  and prove the typed session command reaches the runtime owner.
- The same interaction coverage proves Profile selection does not dismiss the
  drawer, the auxiliary close restores Composer focus, Sessions and Inspector
  switch in place, and outside-click/Escape share the restore path.
- Wide, medium, and narrow captures were visually inspected for unclipped Header
  controls, stable body geometry, and the drawer boundary below the selectors.

VUI-421 makes the Header Thinking selector capability-driven. The ten
deterministic captures were reviewed at wide, medium, narrow, idle,
authorization, reduced-motion, keyboard-focus, and no-color variants and are
accepted as the new closed-Header baseline.

- The fixture reasoning model now presents `Thinking · Auto` / `T · Auto`;
  selectors and panel actions remain on one line without clipping at all three
  viewport widths.
- The normalized diff is confined to the Header labels and their expected
  horizontal redistribution. Conversation, Sidebar, Composer, overlay, focus,
  and no-color geometry are unchanged.
- Capability fixtures and GPUI hit tests separately cover `can_disable`,
  explicit-level ordering, duplicate/illegal values, the non-reasoning hidden
  selector, and the Header-local fallback status.
- Model-switch tests prove that the exact model id and current Thinking value
  enter one typed runtime transaction; unsupported values return as Auto and
  update the session preference only after the accepted response.

VUI-420 adds the Provider-grouped model popup while preserving the reviewed
Desktop shell baseline. All ten deterministic captures compare at normalized
RMSE `0`, so this task does not replace any golden image.

- The closed Header selector retains its existing bounds in wide, medium,
  narrow, idle, authorization, reduced-motion, keyboard-focus, and no-color
  fixtures.
- The opened popup is covered by the native GPUI interaction test: Provider
  labels are non-interactive, the checked model remains keyboard-selectable,
  and the exact typed model id reaches the Home runtime owner.
- Dedicated ViewModel tests cover stable Provider and row ordering, filtered
  unconfigured/image-only entries, bounded long names, current-auth-loss, and
  zero-configured-model states.

VUI-412 established the underlying Desktop visual baseline after the
multi-project Sidebar and Evo Loop asset work.

- `wide-idle`, `medium-idle`, and `narrow-idle` use the production path-only
  wordmark at 360, 320, and 280 logical pixels, retain the confirmed headline
  and supporting copy, and keep the Composer visible at the bottom.
- The shared Composer now uses `What do you want to build or improve?` in Home
  and conversation fixtures.
- Non-idle fixtures retain the reviewed Projects tree and compact Evo mark from
  VUI-410/VUI-411; no unrelated panel geometry changed.
- Authorization, reduced-motion, keyboard-focus, and no-color captures were
  inspected for overlay bounds, focus affordances, static motion behavior, and
  hue-independent hierarchy.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0254532` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0310425` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0279798` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0254532` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0310425` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0279798` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.00354707` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0254532` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0254532` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0293705` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
