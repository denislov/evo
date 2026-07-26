#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"

cd "${repository_root}"
cargo test -p desktop --lib --release desktop_release_ -- --ignored --nocapture --test-threads=1
