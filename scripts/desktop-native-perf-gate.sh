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
markdown_samples_file="$(mktemp)"
trap 'rm -f "${samples_file}" "${markdown_samples_file}"' EXIT

cargo build -p desktop --release --features desktop-devtools
env ZED_MEASUREMENTS=1 EVO_DESKTOP_NATIVE_PERF_REPLAY=1 EVO_DESKTOP_MARKDOWN_TRACE=1 \
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

read -r input_sample_count input_p95_micros input_p99_micros < <(
    awk -F '\t' '
        /native_input_dispatch_to_post_render_p95_us=/ {
            samples = ""
            p95 = ""
            p99 = ""
            for (i = 1; i <= NF; i++) {
                split($i, field, "=")
                if (field[1] == "native_input_samples") samples = field[2]
                if (field[1] == "native_input_dispatch_to_post_render_p95_us") p95 = field[2]
                if (field[1] == "native_input_dispatch_to_post_render_p99_us") p99 = field[2]
            }
            print samples, p95, p99
            exit
        }
    ' "${log_file}"
)

if [[ -z "${input_sample_count:-}" || -z "${input_p95_micros:-}" || -z "${input_p99_micros:-}" ]]; then
    echo "native input-to-post-render metrics were not emitted" >&2
    exit 1
fi

expected_input_samples=50
input_p95_budget_micros=50000
printf 'desktop_perf\tnative_input_samples=%s\tnative_input_dispatch_to_post_render_p95_us=%s\tnative_input_dispatch_to_post_render_p99_us=%s\tnative_input_p95_budget_us=%s\n' \
    "${input_sample_count}" "${input_p95_micros}" "${input_p99_micros}" "${input_p95_budget_micros}" \
    | tee -a "${log_file}"

if [[ "${input_sample_count}" -ne "${expected_input_samples}" ]]; then
    echo "expected ${expected_input_samples} paired native input samples, found ${input_sample_count}" >&2
    exit 1
fi

if [[ "${input_p95_micros}" -gt "${input_p95_budget_micros}" ]]; then
    echo "native input dispatch-to-post-render P95 exceeded 50 ms: ${input_p95_micros} us" >&2
    exit 1
fi

read -r native_platform native_rss_supported native_rss_before native_rss_warmup native_rss_after native_rss_startup_growth native_rss_steady_growth < <(
    awk -F '\t' '
        /native_rss_steady_growth_bytes=/ {
            platform = ""
            supported = ""
            before = ""
            warmup = ""
            after = ""
            startup_growth = ""
            steady_growth = ""
            for (i = 1; i <= NF; i++) {
                split($i, field, "=")
                if (field[1] == "platform") platform = field[2]
                if (field[1] == "native_rss_supported") supported = field[2]
                if (field[1] == "native_rss_before_bytes") before = field[2]
                if (field[1] == "native_rss_after_warmup_bytes") warmup = field[2]
                if (field[1] == "native_rss_after_bytes") after = field[2]
                if (field[1] == "native_rss_startup_growth_bytes") startup_growth = field[2]
                if (field[1] == "native_rss_steady_growth_bytes") steady_growth = field[2]
            }
            print platform, supported, before, warmup, after, startup_growth, steady_growth
            exit
        }
    ' "${log_file}"
)

if [[ -z "${native_platform:-}" || "${native_rss_supported:-}" != "true" ]]; then
    echo "native resident-memory probe is unavailable on this desktop platform" >&2
    exit 1
fi

native_rss_absolute_budget_bytes=$((256 * 1024 * 1024))
native_rss_steady_budget_bytes=$((64 * 1024 * 1024))
printf 'desktop_perf\tplatform=%s\tnative_rss_supported=%s\tnative_rss_before_bytes=%s\tnative_rss_after_warmup_bytes=%s\tnative_rss_after_bytes=%s\tnative_rss_startup_growth_bytes=%s\tnative_rss_steady_growth_bytes=%s\tnative_rss_absolute_budget_bytes=%s\tnative_rss_steady_budget_bytes=%s\n' \
    "${native_platform}" "${native_rss_supported}" "${native_rss_before}" "${native_rss_warmup}" \
    "${native_rss_after}" "${native_rss_startup_growth}" "${native_rss_steady_growth}" \
    "${native_rss_absolute_budget_bytes}" "${native_rss_steady_budget_bytes}" | tee -a "${log_file}"

if [[ "${native_rss_after}" -gt "${native_rss_absolute_budget_bytes}" ]]; then
    echo "native window RSS exceeded 256 MiB: ${native_rss_after} bytes" >&2
    exit 1
fi

if [[ "${native_rss_steady_growth}" -gt "${native_rss_steady_budget_bytes}" ]]; then
    echo "native steady-state RSS growth exceeded 64 MiB: ${native_rss_steady_growth} bytes" >&2
    exit 1
fi

awk -F '\t' '
    /markdown_parse_complete/ {
        for (i = 1; i <= NF; i++) {
            split($i, field, "=")
            if (field[1] == "markdown_parse_to_layout_us") print field[2]
        }
    }
' "${log_file}" | sort -n > "${markdown_samples_file}"

markdown_sample_count="$(wc -l < "${markdown_samples_file}" | tr -d ' ')"
if [[ "${markdown_sample_count}" -lt 1 ]]; then
    echo "production Markdown completion tracing emitted no samples" >&2
    exit 1
fi

markdown_p95_index=$(( (markdown_sample_count * 95 + 99) / 100 ))
markdown_p95_micros="$(sed -n "${markdown_p95_index}p" "${markdown_samples_file}")"
markdown_p95_budget_micros=150000
printf 'desktop_perf\tproduction_markdown_completion_samples=%s\tproduction_markdown_parse_to_layout_p95_us=%s\tproduction_markdown_p95_budget_us=%s\n' \
    "${markdown_sample_count}" "${markdown_p95_micros}" "${markdown_p95_budget_micros}" \
    | tee -a "${log_file}"

if [[ "${markdown_p95_micros}" -gt "${markdown_p95_budget_micros}" ]]; then
    echo "production Markdown parse-to-layout P95 exceeded 150 ms: ${markdown_p95_micros} us" >&2
    exit 1
fi
