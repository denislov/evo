# Evo Loop vector assets

Evo Loop is the Desktop product mark. It is intentionally built from open,
rounded paths rather than text glyphs or a generic AI symbol.

## Geometry

- The full wordmark uses one continuous lowercase `evo` gesture. The `e`
  begins as the seed/initial state, the `v` descends and rises, and the open
  `o` turns inward before its feedback path exits upward.
- The compact mark keeps only that open `o` feedback loop. It has its own
  64×64 path instead of mechanically cropping the 360×128 wordmark, so the
  stroke and negative space remain legible at 16, 24, and 32 px.
- The upward terminal is a separate alpha-mask asset. GPUI applies the accent
  token at render time; monochrome mode deliberately maps it back to the body
  color, leaving the silhouette unchanged.

## Ownership and rendering

- `crates/desktop/src/assets.rs` embeds these files and composes them with the
  pinned `gpui-component-assets` source used for Lucide controls.
- `crates/desktop/src/app/native_shell/evo_brand.rs` owns size, variant, theme
  tokens, accessibility metadata, and the two-layer GPUI rendering contract.
- Product panes consume `EvoBrand`; they do not load files, reproduce paths,
  or choose raw colors.

All four SVG files contain only vector paths. They must not gain `<text>`,
font references, raster images, animation, scripts, filters, or fixed product
colors. The literal black stroke is only an alpha-mask source: GPUI replaces
it with the selected semantic foreground/accent token during painting.
