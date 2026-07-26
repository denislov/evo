#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "desktop native performance gate requires DISPLAY or WAYLAND_DISPLAY" >&2
    exit 2
fi

artifact_dir="${repository_root}/target/desktop-perf"
mkdir -p "${artifact_dir}"
log_file="${artifact_dir}/native-latest.log"
samples_file="$(mktemp)"
trap 'rm -f "${samples_file}"' EXIT

cargo build -p desktop --release
env ZED_MEASUREMENTS=1 EVO_DESKTOP_NATIVE_PERF_REPLAY=1 \
    "${repository_root}/target/release/desktop" 2>&1 | tee "${log_file}"

awk '
    /^frame duration:/ {
        value = $3
        if (value ~ /ms$/) {
            sub(/ms$/, "", value)
            printf "%.0f\n", value * 1000
        } else if (value ~ /µs$/) {
            sub(/µs$/, "", value)
            printf "%.0f\n", value
        } else if (value ~ /ns$/) {
            sub(/ns$/, "", value)
            printf "%.0f\n", value / 1000
        } else if (value ~ /s$/) {
            sub(/s$/, "", value)
            printf "%.0f\n", value * 1000000
        }
    }
' "${log_file}" | tail -n 200 | sort -n > "${samples_file}"

sample_count="$(wc -l < "${samples_file}" | tr -d ' ')"
if [[ "${sample_count}" -ne 200 ]]; then
    echo "expected 200 native frame-duration samples, found ${sample_count}" >&2
    exit 1
fi

p95_index=$(( (sample_count * 95 + 99) / 100 ))
p99_index=$(( (sample_count * 99 + 99) / 100 ))
p95_micros="$(sed -n "${p95_index}p" "${samples_file}")"
p99_micros="$(sed -n "${p99_index}p" "${samples_file}")"
p95_budget_micros=16700
p99_budget_micros=33000
printf 'desktop_perf\tnative_gpu_present_frame_p95_us=%s\tnative_gpu_present_frame_p99_us=%s\tnative_frame_p95_budget_us=%s\tnative_frame_p99_budget_us=%s\n' \
    "${p95_micros}" "${p99_micros}" "${p95_budget_micros}" "${p99_budget_micros}" \
    | tee -a "${log_file}"

if [[ "${p95_micros}" -gt "${p95_budget_micros}" ]]; then
    echo "native GPU/present frame P95 exceeded one frame: ${p95_micros} us" >&2
    exit 1
fi

if [[ "${p99_micros}" -gt "${p99_budget_micros}" ]]; then
    echo "native GPU/present frame P99 exceeded two frames: ${p99_micros} us" >&2
    exit 1
fi
