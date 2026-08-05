#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "${script_dir}/.." && pwd)"
cd -- "${repository_root}"

for command_name in cargo git jq rg sha256sum sort wc; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "architecture baseline requires ${command_name}" >&2
        exit 2
    fi
done

metadata_file="$(mktemp)"
trap 'rm -f "${metadata_file}"' EXIT
cargo metadata --format-version 1 --no-deps > "${metadata_file}"

printf '# Evo architecture baseline\n\n'
printf -- '- Commit: `%s`\n' "$(git rev-parse HEAD)"
printf -- '- Workspace version: `%s`\n' "$(jq -r '.packages[0].version' "${metadata_file}")"
printf -- '- Generator: `scripts/architecture-baseline.sh`\n\n'

printf '## Crate inventory\n\n'
printf '| Crate | Production files | Production LOC | Test files | Test LOC | Test markers | Direct first-party dependencies | Largest production file |\n'
printf '| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |\n'

mapfile -t workspace_packages < <(jq -r '
    . as $root
    | [$root.workspace_members[]] as $member_ids
    | $root.packages[]
    | select(.id as $id | $member_ids | index($id))
    | .name
' "${metadata_file}" | sort)

for package_name in "${workspace_packages[@]}"; do
    manifest_path="$(jq -r --arg package "${package_name}" '
        .packages[] | select(.name == $package) | .manifest_path
    ' "${metadata_file}")"
    package_dir="${manifest_path%/Cargo.toml}"
    if [[ "${package_dir}" == "${repository_root}" ]]; then
        package_dir='.'
    else
        package_dir="${package_dir#${repository_root}/}"
    fi

    production_files=0
    production_loc=0
    test_files=0
    test_loc=0
    test_markers=0
    largest_lines=0
    largest_path='-'

    mapfile -t package_rust_files < <(
        for candidate_root in "${package_dir}/src" "${package_dir}/tests"; do
            if [[ -d "${candidate_root}" ]]; then
                rg --files "${candidate_root}" -g '*.rs'
            fi
        done | sort -u
    )

    for source_file in "${package_rust_files[@]}"; do
        line_count="$(wc -l < "${source_file}")"
        marker_count="$(rg -c '^\s*#\[(tokio::)?test\]' "${source_file}" || true)"
        marker_count="${marker_count:-0}"
        test_markers=$((test_markers + marker_count))
        case "${source_file}" in
            */tests/*|*_tests.rs|*/test_support.rs)
                test_files=$((test_files + 1))
                test_loc=$((test_loc + line_count))
                ;;
            *)
                production_files=$((production_files + 1))
                production_loc=$((production_loc + line_count))
                if ((line_count > largest_lines)); then
                    largest_lines="${line_count}"
                    largest_path="${source_file}"
                fi
                ;;
        esac
    done

    dependencies="$(jq -r --arg package "${package_name}" '
        . as $root
        | [$root.workspace_members[]] as $member_ids
        | [$root.packages[]
            | select(.id as $id | $member_ids | index($id))
            | .name] as $member_names
        | $root.packages[]
        | select(.name == $package)
        | [.dependencies[]
            | select(.kind != "dev")
            | select(.path != null)
            | select(.name as $dependency | $member_names | index($dependency))
            | .name]
        | unique
        | if length == 0 then "-" else join(", ") end
    ' "${metadata_file}")"

    if [[ "${largest_path}" == '-' ]]; then
        largest='-'
    else
        largest="${largest_path} (${largest_lines})"
    fi
    printf '| `%s` | %s | %s | %s | %s | %s | %s | `%s` |\n' \
        "${package_name}" "${production_files}" "${production_loc}" \
        "${test_files}" "${test_loc}" "${test_markers}" "${dependencies}" "${largest}"
done

printf '\n## First-party dependency edges\n\n```text\n'
jq -r '
    . as $root
    | [$root.workspace_members[]] as $member_ids
    | [$root.packages[]
        | select(.id as $id | $member_ids | index($id))
        | .name] as $member_names
    | $root.packages[]
    | select(.id as $id | $member_ids | index($id))
    | .name as $source
    | .dependencies[]
    | select(.kind != "dev")
    | select(.path != null)
    | select(.name as $dependency | $member_names | index($dependency))
    | "\($source) -> \(.name)"
' "${metadata_file}" | sort -u
printf '```\n\n'

printf '## Oversized Rust files\n\n'
printf '| Kind | Limit | Lines | Path |\n| --- | ---: | ---: | --- |\n'
mapfile -t all_rust_files < <(rg --files crates -g '*.rs' | sort)
for source_file in "${all_rust_files[@]}"; do
    case "${source_file}" in
        */tests/*|*_tests.rs|*/test_support.rs)
            kind=test
            limit=1200
            ;;
        *)
            kind=production
            limit=900
            ;;
    esac
    line_count="$(wc -l < "${source_file}")"
    if ((line_count > limit)); then
        printf '| %s | %s | %s | `%s` |\n' "${kind}" "${limit}" "${line_count}" "${source_file}"
    fi
done

printf '\n## Contract fixtures\n\n'
printf '| Bytes | SHA-256 | Path |\n| ---: | --- | --- |\n'
while IFS= read -r fixture; do
    printf '| %s | `%s` | `%s` |\n' \
        "$(wc -c < "${fixture}" | tr -d ' ')" \
        "$(sha256sum "${fixture}" | awk '{print $1}')" \
        "${fixture}"
done < <(find crates/coding-agent/tests/fixtures -type f | sort)
