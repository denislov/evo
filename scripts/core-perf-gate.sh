#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "${script_dir}/.." && pwd)"
artifact_dir="${repository_root}/target/core-perf"

cd -- "${repository_root}"
mkdir -p "${artifact_dir}"

{
    cargo test --locked -p agent-core --features test-support --lib --release \
        agent::turn::runtime::loop_tests::agent_core_release_faux_first_text_delta_baseline -- \
        --ignored --exact --nocapture --test-threads=1

    cargo test --locked -p coding-agent --lib --release \
        session::repository::bounded::tests::hundred_thousand_event_hydration_read_is_time_and_memory_bounded -- \
        --exact --nocapture --test-threads=1

    cargo test --locked -p coding-agent --lib --release \
        platform::process::tests::noisy_output_is_bounded_and_updates_are_throttled -- \
        --exact --nocapture --test-threads=1
} 2>&1 | tee "${artifact_dir}/latest.log"
