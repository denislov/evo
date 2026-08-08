# GPUI / gpui-component provenance

Status: adapted dependency with tracked local patch.

Upstream repositories:

- `https://github.com/longbridge/gpui-component.git`
- `https://github.com/zed-industries/zed.git`

Pinned revisions:

- gpui-component base: `bc174a7ec4534b2a4174fddde314b38d30d69093`
- Zed/GPUI lockfile revision: `30730a305ae235f3be44643d5895e142048ef701`

Source paths:

- Recreated checkout: `third-party/gpui-component/`（gitignored）
- Build declarations: `crates/desktop/Cargo.toml`
- Reproduction script: `scripts/vendor-gpui-component.sh`
- Local patch: `patches/gpui-component/0001-text-seed-the-background-parse-accumulator-from-the-.patch`

License/notices:

- gpui-component: Apache-2.0, upstream `LICENSE-APACHE`
- GPUI crates with Cargo metadata: Apache-2.0 unless their manifest states otherwise
- Zed repository also contains GPL-licensed tracing crates in the resolved graph; exact metadata inventory and release review note are recorded in `THIRD_PARTY_NOTICES.md`

Local modifications:

- The patch seeds gpui-component's background text parser from the synchronous `set_text` result so a later streaming `push_str` appends instead of replacing prior content.
- The source tree itself is not tracked; CI and developers recreate it from the pinned revision and apply the tracked patch deterministically.

Tests:

- TUI/Desktop streaming Markdown and native replay tests exercise the mixed synchronous/incremental text path.
- `scripts/vendor-gpui-component.sh` fails if the pinned checkout or patch application fails.

Sync policy: one pinned upstream revision at a time. Upgrades must refresh this record, rebase or delete every local patch, run Desktop/TUI tests and update notices before merge.

