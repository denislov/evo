#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
artifact_dir="${repository_root}/target/release-api-snapshots"

cd "${repository_root}"
mkdir -p "${artifact_dir}"

# Release API inventory for every stable workspace facade and serialized contract.
# Each command names a real facade or serialized contract test. Keeping the list
# explicit makes a newly exposed surface a reviewed release edit.
{
    printf 'release_api\tpackage=agent-core\ttarget=api_contract\n'
    cargo test --locked -p agent-core --test api_contract

    printf 'release_api\tpackage=ai-protocol\ttarget=api_contract\n'
    cargo test --locked -p ai-protocol --test api_contract

    printf 'release_api\tpackage=ai-protocol\ttarget=protocol_contract\n'
    cargo test --locked -p ai-protocol --test protocol_contract

    printf 'release_api\tpackage=ai\ttarget=api_contract\n'
    cargo test --locked -p ai --test api_contract

    printf 'release_api\tpackage=tool-contract\ttarget=api_contract\n'
    cargo test --locked -p tool-contract

    printf 'release_api\tpackage=tool-runtime\ttarget=api_contract\n'
    cargo test --locked -p tool-runtime

    printf 'release_api\tpackage=event-journal\ttarget=api_contract\n'
    cargo test --locked -p event-journal

    printf 'release_api\tpackage=workspace-runtime\ttarget=api_contract\n'
    cargo test --locked -p workspace-runtime --test api_contract

    printf 'release_api\tpackage=observability\ttarget=api_contract\n'
    cargo test --locked -p observability --test api_contract

    printf 'release_api\tpackage=release-updater\ttarget=public_contract\n'
    cargo test --locked -p release-updater --all-targets

    printf 'release_api\tpackage=coding-agent\ttarget=api_contract\n'
    cargo test --locked -p coding-agent --test api_contract

    printf 'release_api\tpackage=coding-agent\ttarget=product_event_projection_golden\n'
    cargo test --locked -p coding-agent --lib domain::projection::golden

    printf 'release_api\tpackage=coding-agent\ttarget=operation_and_capability_contracts\n'
    cargo test --locked -p coding-agent --lib application::operation::tests
    cargo test --locked -p coding-agent --lib operations::delegation::capability_snapshot_tests

    printf 'release_api\tpackage=desktop\ttarget=dependency_boundary\n'
    cargo test --locked -p desktop --test dependency_boundary

    printf 'release_api\tpackage=tui\ttarget=api_contract\n'
    cargo test --locked -p tui --features test-support --test api_contract
} 2>&1 | tee "${artifact_dir}/latest.log"
