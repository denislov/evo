#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "click-to-photon replay requires DISPLAY or WAYLAND_DISPLAY" >&2
    exit 2
fi

artifact_dir="${repository_root}/target/desktop-perf"
mkdir -p "${artifact_dir}"
log_file="${artifact_dir}/click-to-photon-app-latest.log"

cargo build -p desktop --release
env EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY=1 \
    "${repository_root}/target/release/desktop" 2>&1 | tee "${log_file}"
