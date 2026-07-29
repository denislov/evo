# Native visual golden review

## VUI-305 review note

Target surface: the Composer in active-session and idle/new-task workspaces.

The Composer now separates prompt entry from its action row. A visible `+` attachment action sits at the lower left and the send action remains at the lower right; running prompts keep their steer/queue selector adjacent to send. Selected attachment chips appear between the input and footer with a filename and an individually accessible remove action. The thinking selector remains exclusively in the Header after VUI-304.

- Wide: the new footer is balanced beneath the input with the attachment and send actions at opposite edges; docked Sessions and Inspector panels remain unchanged.
- Medium: the footer and input stay bounded with the Sessions panel docked, and the Composer retains its existing auto-grow ceiling.
- Narrow: the two-row organization prevents the attachment action from competing horizontally with the input; the send action remains reachable without clipping.
- Idle wide/medium/narrow: the same attachment affordance is available before the first prompt and follows the eventual session workspace.
- Accessibility: the attachment picker has the label `Add files or images`; unsupported models expose the disabled reason in the label and visible metadata. Each selected file is a list item labelled with its path and has a named remove button. Keyboard-focus and no-color fixtures preserve state without relying on hue.

Authorization and reduced-motion fixtures retain their existing overlay behavior. The visual change is limited to the Composer input/footer organization and the new attachment affordance.

# Native visual before/after review

Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with `--update --review-note FILE`.

| Fixture | Size | Normalized RMSE | Before | After | Diff |
| --- | ---: | ---: | --- | --- | --- |
| `wide` | `2600x1656` | `0.12577` | `wide-before.png` | `wide-after.png` | `wide-diff.png` |
| `medium` | `1800x1600` | `0.148687` | `medium-before.png` | `medium-after.png` | `medium-diff.png` |
| `narrow` | `1400x1600` | `0.0189556` | `narrow-before.png` | `narrow-after.png` | `narrow-diff.png` |
| `wide-idle` | `2600x1656` | `0.0202384` | `wide-idle-before.png` | `wide-idle-after.png` | `wide-idle-diff.png` |
| `medium-idle` | `1800x1600` | `0.0247455` | `medium-idle-before.png` | `medium-idle-after.png` | `medium-idle-diff.png` |
| `narrow-idle` | `1400x1600` | `0.0795354` | `narrow-idle-before.png` | `narrow-idle-after.png` | `narrow-idle-diff.png` |
| `wide-authorization` | `2600x1656` | `0.0105601` | `wide-authorization-before.png` | `wide-authorization-after.png` | `wide-authorization-diff.png` |
| `wide-reduced-motion` | `2600x1656` | `0.125815` | `wide-reduced-motion-before.png` | `wide-reduced-motion-after.png` | `wide-reduced-motion-diff.png` |
| `wide-keyboard-focus` | `2600x1656` | `0.125825` | `wide-keyboard-focus-before.png` | `wide-keyboard-focus-after.png` | `wide-keyboard-focus-diff.png` |
| `wide-no-color` | `2600x1656` | `0.143945` | `wide-no-color-before.png` | `wide-no-color-after.png` | `wide-no-color-diff.png` |
