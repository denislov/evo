# Native visual golden review

VUI-412 finalizes the reviewed Desktop visual baseline after the multi-project
Sidebar and Evo Loop asset work.

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
| `wide` | `2600x1656` | `0.14088` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0615561` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.126563` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.122258` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.134082` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.137558` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0246912` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.139742` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.139749` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.161142` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
