#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
artifact_dir="${repository_root}/target/release-api-snapshots"

cd "${repository_root}"
mkdir -p "${artifact_dir}"

# Release API inventory: agent-core ai coding-agent desktop tui.
# Each target owns compile-time facade checks or serialized contract snapshots;
# keeping the list explicit makes a newly exposed surface a reviewed release edit.
{
    printf 'release_api\tpackage=agent-core\ttarget=api_contract\n'
    cargo test --locked -p agent-core --features test-support --test api_contract

    printf 'release_api\tpackage=ai\ttargets=public_api,api_boundary_guards,provider_registry_boundary_guards\n'
    cargo test --locked -p ai \
        --test public_api \
        --test api_boundary_guards \
        --test provider_registry_boundary_guards

    printf 'release_api\tpackage=coding-agent\ttargets=api_contract,events_snapshot\n'
    cargo test --locked -p coding-agent \
        --test api_contract \
        --test events_snapshot

    printf 'release_api\tpackage=desktop\ttarget=dependency_boundary\n'
    cargo test --locked -p desktop --test dependency_boundary

    printf 'release_api\tpackage=tui\ttarget=api_contract\n'
    cargo test --locked -p tui --features test-support --test api_contract
} 2>&1 | tee "${artifact_dir}/latest.log"
