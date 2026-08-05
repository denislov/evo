#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "${script_dir}/.." && pwd)"
artifact_dir="${repository_root}/target/core-perf"

cd -- "${repository_root}"
mkdir -p "${artifact_dir}"

run_exact_release_test() {
    local package="$1"
    local filter="$2"
    local run_ignored="$3"
    shift 3

    local command=(
        cargo test --locked -p "${package}" "$@" --lib --release "${filter}" --
        --exact --nocapture --test-threads=1
    )
    if [[ "${run_ignored}" == true ]]; then
        command+=(--ignored)
    fi

    local output
    if ! output="$("${command[@]}" 2>&1)"; then
        printf '%s\n' "${output}"
        return 1
    fi
    printf '%s\n' "${output}"
    if ! rg -q '^running 1 test$' <<<"${output}"; then
        printf 'core performance test filter matched an unexpected test count: %s\n' \
            "${filter}" >&2
        return 1
    fi
}

{
    run_exact_release_test agent-core \
        agent::turn::runtime::loop_tests::agent_core_release_faux_first_text_delta_baseline \
        true --features test-support

    run_exact_release_test coding-agent \
        session::repository::bounded::tests::hundred_thousand_event_hydration_read_is_time_and_memory_bounded \
        false

    run_exact_release_test workspace-runtime \
        process::tests_file::tests::noisy_output_is_bounded_and_updates_are_throttled \
        false
} 2>&1 | tee "${artifact_dir}/latest.log"
