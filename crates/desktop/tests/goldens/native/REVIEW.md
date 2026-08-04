# Native visual golden review

Reviewed the deterministic desktop visual replay after the conversation UI refresh.

Expected changes:

- the application now uses a warm light palette with accessible text and status contrast;
- the conversation header is taller, white, and visually separated from the transcript;
- transcript rows have more breathing room, with flat reasoning and tool rails instead of heavy cards;
- the Composer is an elevated rounded surface with wider outer margins;
- wide, medium, narrow, authorization, drawer, Home, and no-color fixtures remain bounded and legible.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.74918` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.739141` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.742269` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.764931` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.757487` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.772096` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.359695` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.749067` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.74905` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `medium-inspector` | `1800x1600` | `0.728787` | `medium-inspector-before.png` | `medium-inspector-after.png` | `medium-inspector-diff.png` |
| `narrow-inspector` | `1400x1600` | `0.729778` | `narrow-inspector-before.png` | `narrow-inspector-after.png` | `narrow-inspector-diff.png` |
| `wide-model-menu` | `2600x1656` | `0.764518` | `wide-model-menu-before.png` | `wide-model-menu-after.png` | `wide-model-menu-diff.png` |
| `wide-thinking-menu` | `2600x1656` | `0.764869` | `wide-thinking-menu-before.png` | `wide-thinking-menu-after.png` | `wide-thinking-menu-diff.png` |
| `wide-thinking-non-reasoning` | `2600x1656` | `0.764498` | `wide-thinking-non-reasoning-before.png` | `wide-thinking-non-reasoning-after.png` | `wide-thinking-non-reasoning-diff.png` |
| `wide-home-project` | `2600x1656` | `0.764775` | `wide-home-project-before.png` | `wide-home-project-after.png` | `wide-home-project-diff.png` |
| `wide-home-long-project` | `2600x1656` | `0.764475` | `wide-home-long-project-before.png` | `wide-home-long-project-after.png` | `wide-home-long-project-diff.png` |
| `wide-catalog-loading` | `2600x1656` | `0.765036` | `wide-catalog-loading-before.png` | `wide-catalog-loading-after.png` | `wide-catalog-loading-diff.png` |
| `wide-catalog-error` | `2600x1656` | `0.764951` | `wide-catalog-error-before.png` | `wide-catalog-error-after.png` | `wide-catalog-error-diff.png` |
| `wide-catalog-empty` | `2600x1656` | `0.765028` | `wide-catalog-empty-before.png` | `wide-catalog-empty-after.png` | `wide-catalog-empty-diff.png` |
| `wide-no-color` | `2600x1656` | `0.871357` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
