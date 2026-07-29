#!/usr/bin/env bash
# Recreate the patched `gpui-component` checkout that the workspace `[patch]`
# section in the root Cargo.toml builds against.
#
# The vendored tree is deliberately untracked (see .gitignore): only this script
# and patches/gpui-component/*.patch are committed, so the repository stays small
# and the delta from upstream stays reviewable.
#
# To add a patch: commit it in third-party/gpui-component, then re-export with
#   git -C third-party/gpui-component format-patch -o ../../patches/gpui-component "$UPSTREAM_REV"..HEAD
#
# To drop the patches entirely: delete the [patch] section from the root
# Cargo.toml and the build falls straight back to the upstream revision.
set -euo pipefail

# Must match the gpui-component rev in crates/desktop/Cargo.toml. The
# `unstable_ui_dependencies_are_exactly_pinned` boundary test enforces that.
UPSTREAM_URL="https://github.com/longbridge/gpui-component.git"
UPSTREAM_REV="bc174a7ec4534b2a4174fddde314b38d30d69093"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
vendor_dir="$repo_root/third-party/gpui-component"
patch_dir="$repo_root/patches/gpui-component"

if [[ -e "$vendor_dir" ]]; then
  echo "error: $vendor_dir already exists; remove it first to re-vendor" >&2
  exit 1
fi

mkdir -p "$(dirname "$vendor_dir")"

# Prefer the local cargo git database so this works offline: cargo already
# fetched this exact revision to build the unpatched dependency.
cargo_db="$(find "${CARGO_HOME:-$HOME/.cargo}/git/db" -maxdepth 1 -type d \
  -name 'gpui-component-*' 2>/dev/null | head -n 1 || true)"

if [[ -n "$cargo_db" ]] && git -C "$cargo_db" cat-file -e "$UPSTREAM_REV^{commit}" 2>/dev/null; then
  echo "vendoring from local cargo database: $cargo_db"
  git clone --quiet "$cargo_db" "$vendor_dir"
else
  echo "vendoring from $UPSTREAM_URL"
  git clone --quiet "$UPSTREAM_URL" "$vendor_dir"
fi

git -C "$vendor_dir" checkout --quiet -b evo-patches "$UPSTREAM_REV"

shopt -s nullglob
patches=("$patch_dir"/*.patch)
shopt -u nullglob

if (( ${#patches[@]} == 0 )); then
  echo "no patches in $patch_dir; vendored tree is plain upstream $UPSTREAM_REV"
  exit 0
fi

git -C "$vendor_dir" -c user.name="evo" -c user.email="evo@local" am "${patches[@]}"

echo
echo "vendored $UPSTREAM_REV + ${#patches[@]} patch(es) into $vendor_dir"
git -C "$vendor_dir" log --oneline "$UPSTREAM_REV..HEAD"
