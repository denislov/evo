#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"

cd "${repository_root}"
artifact_dir="${repository_root}/target/desktop-perf"
mkdir -p "${artifact_dir}"
cargo test -p desktop --lib --release desktop_release_ -- --ignored --nocapture --test-threads=1 \
    2>&1 | tee "${artifact_dir}/latest.log"
