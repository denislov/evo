# Native visual golden review

## VUI-308 review note

Target surface: durable and live conversation blocks in the central Conversation pane.

Conversation blocks no longer use role-tinted rounded card backgrounds. Role identity is carried by the leading `YOU` / `AI` / `TOOL` / `AGENT` / `SUMMARY` / `ISSUE` marker and its semantic text tone; the marker keeps its former transparent padding so measured row geometry does not change. Nested reasoning, tool-detail, code, and secondary-content surfaces retain their elevated fill because those surfaces still communicate disclosure hierarchy.

Selection and hover no longer repaint the block background. A selected row uses a two-pixel full-height leading rail. An unselected hovered row uses the same reserved, absolutely positioned slot but paints only a short centered rail. The different lengths keep the states distinct in grayscale and without relying on hue, while neither state changes card bounds or the virtual-list height measurement.

- Wide: card fills and rounded shells are gone across diagnostic, assistant, and tool rows; the full-height selected tool rail is visible, role markers remain legible, and the nested reasoning/code surfaces retain their hierarchy.
- Medium: the same lighter block treatment remains bounded between the Sessions pane and composer; no content or action is clipped.
- Narrow: conversation rows keep the new flat treatment behind the responsive Sessions overlay, with stable wrapping and bottom anchoring.
- Idle wide/medium/narrow: no Conversation pane is mounted, so Home layout and geometry are unchanged.
- Authorization: the underlying flat Conversation hierarchy remains readable beneath the modal and the authorization focus treatment is unchanged.
- Reduced motion: the selected rail and flat block hierarchy match the standard fixture; no motion-dependent carrier was introduced.
- Keyboard focus: the full-height selection rail remains visible together with the existing keyboard-focus affordance.
- No color: the grayscale fixture preserves the full-height selected rail, role text hierarchy, and nested-detail boundaries. A GPUI pointer regression separately verifies that the short hover rail is less than half the selected rail height and that both states preserve the exact card bounds.

Accessibility semantics are unchanged: conversation rows remain list items with `aria-selected`, position, set size, expansion, and active-descendant metadata. Selection and hover are expressed by geometry as well as tone, and the existing focus path remains keyboard-visible.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.0405666` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.0223601` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.00348118` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0049725` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.0405666` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.0405666` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.0460273` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
