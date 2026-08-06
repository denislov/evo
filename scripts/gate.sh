#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

scripts/architecture-gate.sh

if rg -n -U \
    --glob '*.rs' \
    --glob '!mutex.rs' \
    '\.lock\(\)\s*\.(unwrap|expect|unwrap_or_else)\(' \
    crates/coding-agent/src crates/coding-agent/tests
then
    echo 'coding-agent mutex locks must use the crate-wide poison policy' >&2
    exit 1
fi

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
