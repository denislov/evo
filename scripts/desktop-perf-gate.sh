#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"

cd "${repository_root}"
artifact_dir="${repository_root}/target/desktop-perf"
mkdir -p "${artifact_dir}"

release_tests=(
    "conversation::model::tests::desktop_release_empty_conversation_baseline"
    "conversation::model::tests::desktop_release_ten_mib_interaction_baseline"
    "conversation::model::tests::desktop_release_scale_content_and_streaming_matrix"
    "app::native_shell::tests::desktop_release_gpui_headless_frame_and_input_replay"
    "app::native_shell::tests::desktop_release_gpui_markdown_parser_matrix"
)

run_release_test() {
    local test_name="$1"
    local output

    if ! output="$(
        cargo test -p desktop --lib --release "${test_name}" -- \
            --ignored --exact --nocapture --test-threads=1 2>&1
    )"; then
        printf '%s\n' "${output}"
        return 1
    fi
    printf '%s\n' "${output}"
    if [[ "${output}" != *"running 1 test"* ]]; then
        printf 'desktop performance gate expected exactly one test for %s\n' \
            "${test_name}" >&2
        return 1
    fi
}

{
    for test_name in "${release_tests[@]}"; do
        run_release_test "${test_name}"
    done
} 2>&1 | tee "${artifact_dir}/latest.log"
