#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

if rg -n -U \
    --glob '*.rs' \
    --glob '!mutex.rs' \
    '\.lock\(\)\s*\.(unwrap|expect|unwrap_or_else)\(' \
    crates/coding-agent/src crates/coding-agent/tests
then
    echo 'coding-agent mutex locks must use the crate-wide poison policy' >&2
    exit 1
fi

oversized_rust_file=0
while IFS= read -r source_file; do
    line_count="$(wc -l < "$source_file")"
    if ((line_count > 900)); then
        printf '%s:%s exceeds the 900-line source limit\n' "$source_file" "$line_count" >&2
        oversized_rust_file=1
    fi
done < <(rg --files crates/coding-agent/src crates/coding-agent/tests -g '*.rs')
if ((oversized_rust_file != 0)); then
    exit 1
fi

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
