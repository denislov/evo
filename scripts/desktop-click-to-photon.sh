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
minimum_samples=50

cargo build -p desktop --release
echo "Press Space at least ${minimum_samples} times while the external sensor records matching sample IDs; press Escape only after the final post-render sample."
echo "The external CSV must contain run_id,sample_id,latency_us and use the run ID printed by this replay."
env EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY=1 \
    "${repository_root}/target/release/desktop" 2>&1 | tee "${log_file}"

run_summary="$(
    awk -F '\t' '
        $2 == "click_to_photon_post_render" && $3 ~ /^run=/ {
            runs[substr($3, 5)] = 1
        }
        END {
            for (run in runs) {
                count += 1
                run_id = run
            }
            print count + 0 "\t" run_id
        }
    ' "${log_file}"
)"
run_count="${run_summary%%$'\t'*}"
run_id="${run_summary#*$'\t'}"
if (( run_count != 1 )); then
    echo "click-to-photon replay must emit exactly one run ID; found ${run_count}" >&2
    exit 1
fi

sample_count="$(
    awk -F '\t' -v expected_run="${run_id}" '
        $2 == "click_to_photon_post_render" &&
        $3 == "run=" expected_run &&
        $4 ~ /^sample=[0-9]+$/ {
            samples[substr($4, 8)] = 1
        }
        END {
            for (sample in samples) {
                count += 1
            }
            print count + 0
        }
    ' "${log_file}"
)"
if (( sample_count < minimum_samples )); then
    echo "click-to-photon replay requires at least ${minimum_samples} post-render samples; found ${sample_count}" >&2
    exit 1
fi
echo "click-to-photon replay captured ${sample_count} unique post-render samples for run ${run_id} in ${log_file}"
