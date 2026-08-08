# Evo third-party notices and audit record

Last reviewed: 2026-08-08.

This file is an inventory and provenance record, not legal advice. Release owners remain responsible for confirming that the selected distribution model satisfies every dependency license.

## Direct git and vendored sources

### gpui-component

- Upstream: `longbridge/gpui-component`
- Revision: `bc174a7ec4534b2a4174fddde314b38d30d69093`
- License: Apache-2.0
- Local source: recreated under ignored `third-party/gpui-component/`
- Local modification: tracked patch under `patches/gpui-component/`
- Full provenance: `docs/refactor/provenance/gpui-component.md`

### Zed / GPUI

- Upstream: `zed-industries/zed`
- Resolved revision: `30730a305ae235f3be44643d5895e142048ef701`
- Most GPUI packages report Apache-2.0 in Cargo metadata.
- `gpui_shared_string` and `gpui_util` omit the package-level license field; the upstream repository contains `LICENSE-APACHE` and `LICENSE-GPL`. This missing-metadata exception is pinned in `scripts/licenses/metadata-exceptions.tsv`.
- The resolved all-target graph also contains `zlog`, `ztracing` and `ztracing_macro` reporting GPL-3.0-or-later. They arrive through `sum_tree -> gpui`. This must remain visible in release review; the mechanical audit does not declare proprietary distribution compatibility.
- Cargo metadata also contains optional-choice expressions such as `MIT OR Apache-2.0 OR LGPL-2.1-or-later` (`r-efi`) and `Apache-2.0 OR GPL-2.0-only` (`self_cell`). The audit reports every expression containing GPL/LGPL text; an `OR` expression is not treated as a forced copyleft election by the script.

### Other git dependencies

Cargo metadata currently records:

- `proptest-rs/proptest`: MIT OR Apache-2.0
- `zed-industries/xim-rs`: MIT
- `zed-industries/font-kit`: MIT OR Apache-2.0
- `zed-industries/reqwest`: MIT OR Apache-2.0
- `zed-industries/scap`: MIT

Their exact revisions are locked in `Cargo.lock`; `scripts/license-audit.sh` fails on a new missing license field unless it is explicitly reviewed.

### HTML to Markdown

- `htmd 0.5.5`: Apache-2.0.
- ARC-1030 removed the former direct `html2md` dependency after Cargo metadata identified it as GPL-3.0+.

### Grok build study tree

- Upstream checkout: `third-party/grok-build`（ignored, architecture-study input）
- License: Apache-2.0 with upstream third-party notices
- Evo does not link the Grok workspace.
- Adapted implementation records: `docs/refactor/provenance/grok-build.md`, `codex.md`, `opencode.md`

## crates.io dependencies

The complete transitive list is determined by `Cargo.lock` and Cargo metadata. Run:

```bash
scripts/license-audit.sh
```

The script requires every resolved package to expose a license expression or appear in the reviewed missing-metadata exception table. It also verifies that required provenance and local patch records exist.

## Local proprietary code

Workspace crates use SPDX expression `LicenseRef-Proprietary`. The custom license text is stored at `LICENSES/LicenseRef-Proprietary.txt`.
