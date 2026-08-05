#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "${script_dir}/.." && pwd)"
architecture_dir="${script_dir}/architecture"
oversized_debt_file="${architecture_dir}/oversized-rust-debt.tsv"
dependency_allowlist_file="${architecture_dir}/internal-dependencies.tsv"
execution_debt_file="${architecture_dir}/execution-debt.tsv"
final_mode=false

if [[ "${1:-}" == "--final" ]]; then
    final_mode=true
elif [[ $# -ne 0 ]]; then
    echo "usage: scripts/architecture-gate.sh [--final]" >&2
    exit 2
fi

cd -- "${repository_root}"

for command_name in cargo jq rg sort tsort wc; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "architecture gate requires ${command_name}" >&2
        exit 2
    fi
done

for required_file in \
    "${oversized_debt_file}" \
    "${dependency_allowlist_file}" \
    "${execution_debt_file}"
do
    if [[ ! -f "${required_file}" ]]; then
        echo "architecture gate is missing ${required_file#${repository_root}/}" >&2
        exit 1
    fi
done

failure=0

declare -A debt_kind=()
declare -A debt_limit=()
declare -A debt_max_lines=()
declare -A debt_clear_by=()
declare -A debt_seen=()

while IFS=$'\t' read -r kind limit max_lines clear_by path; do
    if [[ -z "${kind}" || "${kind}" == \#* ]]; then
        continue
    fi
    if [[ "${kind}" != "production" && "${kind}" != "test" ]]; then
        echo "invalid oversized debt kind '${kind}' for ${path}" >&2
        failure=1
        continue
    fi
    if [[ ! "${limit}" =~ ^[0-9]+$ || ! "${max_lines}" =~ ^[0-9]+$ ]]; then
        echo "invalid oversized debt limits for ${path}" >&2
        failure=1
        continue
    fi
    if [[ -n "${debt_kind[${path}]+x}" ]]; then
        echo "duplicate oversized debt entry for ${path}" >&2
        failure=1
        continue
    fi
    debt_kind["${path}"]="${kind}"
    debt_limit["${path}"]="${limit}"
    debt_max_lines["${path}"]="${max_lines}"
    debt_clear_by["${path}"]="${clear_by}"
done < "${oversized_debt_file}"

mapfile -t rust_files < <(rg --files crates -g '*.rs' | sort)
for source_file in "${rust_files[@]}"; do
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
        if [[ -z "${debt_kind[${source_file}]+x}" ]]; then
            printf '%s:%s exceeds the %s-line %s limit and has no debt entry\n' \
                "${source_file}" "${line_count}" "${limit}" "${kind}" >&2
            failure=1
            continue
        fi
        debt_seen["${source_file}"]=1
        if [[ "${debt_kind[${source_file}]}" != "${kind}" || "${debt_limit[${source_file}]}" != "${limit}" ]]; then
            echo "${source_file}: oversized debt classification no longer matches the gate" >&2
            failure=1
        fi
        if ((line_count > debt_max_lines[${source_file}])); then
            printf '%s:%s grew past its debt baseline %s (clear by %s)\n' \
                "${source_file}" "${line_count}" "${debt_max_lines[${source_file}]}" \
                "${debt_clear_by[${source_file}]}" >&2
            failure=1
        fi
    elif [[ -n "${debt_kind[${source_file}]+x}" ]]; then
        printf '%s is now within its %s-line limit; remove the stale debt entry\n' \
            "${source_file}" "${limit}" >&2
        failure=1
    fi
done

for debt_path in "${!debt_kind[@]}"; do
    if [[ ! -f "${debt_path}" ]]; then
        echo "oversized debt references missing file ${debt_path}" >&2
        failure=1
    elif [[ -z "${debt_seen[${debt_path}]+x}" ]]; then
        echo "oversized debt for ${debt_path} is stale" >&2
        failure=1
    fi
done

metadata_file="$(mktemp)"
actual_dependencies="$(mktemp)"
expected_dependencies="$(mktemp)"
todo_occurrences="$(mktemp)"
trap 'rm -f "${metadata_file}" "${actual_dependencies}" "${expected_dependencies}" "${todo_occurrences}"' EXIT

cargo metadata --format-version 1 --no-deps > "${metadata_file}"
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
    | "\($source)\t\(.name)"
' "${metadata_file}" | sort -u > "${actual_dependencies}"

awk -F '\t' 'NF && $1 !~ /^#/ { print $1 "\t" $2 }' \
    "${dependency_allowlist_file}" | sort -u > "${expected_dependencies}"

if ! diff -u "${expected_dependencies}" "${actual_dependencies}"; then
    echo "first-party dependency graph differs from scripts/architecture/internal-dependencies.tsv" >&2
    failure=1
fi

if [[ -s "${actual_dependencies}" ]] && ! tr '\t' ' ' < "${actual_dependencies}" | tsort >/dev/null; then
    echo "first-party dependency graph contains a cycle" >&2
    failure=1
fi

if facade_violations="$(rg -n --pcre2 \
    'coding_agent::(?!api(?:::|\b))' \
    crates/cli/src crates/desktop/src -g '*.rs' || true)" \
    && [[ -n "${facade_violations}" ]]
then
    echo "CLI/Desktop must access coding-agent only through coding_agent::api:" >&2
    echo "${facade_violations}" >&2
    failure=1
fi

declare -A execution_phase=()
declare -A execution_path=()
declare -A execution_seen=()
while IFS=$'\t' read -r debt_id phase path description; do
    if [[ -z "${debt_id}" || "${debt_id}" == \#* ]]; then
        continue
    fi
    if [[ ! "${debt_id}" =~ ^ARC-D[0-9]{3}$ || ! "${phase}" =~ ^[0-9]+$ || -z "${path}" || -z "${description}" ]]; then
        echo "invalid execution debt entry: ${debt_id}" >&2
        failure=1
        continue
    fi
    if [[ -n "${execution_phase[${debt_id}]+x}" ]]; then
        echo "duplicate execution debt id ${debt_id}" >&2
        failure=1
        continue
    fi
    execution_phase["${debt_id}"]="${phase}"
    execution_path["${debt_id}"]="${path}"
done < "${execution_debt_file}"

rg -n 'TODO\(ARC-' crates -g '*.rs' > "${todo_occurrences}" || true
while IFS= read -r occurrence; do
    if [[ ! "${occurrence}" =~ ^([^:]+):([0-9]+):.*TODO\((ARC-D[0-9]{3}),\ Phase\ ([0-9]+)\): ]]; then
        echo "invalid ARC debt marker; expected TODO(ARC-DNNN, Phase N): ${occurrence}" >&2
        failure=1
        continue
    fi
    path="${BASH_REMATCH[1]}"
    debt_id="${BASH_REMATCH[3]}"
    phase="${BASH_REMATCH[4]}"
    if [[ -z "${execution_phase[${debt_id}]+x}" ]]; then
        echo "${path}: unregistered execution debt ${debt_id}" >&2
        failure=1
        continue
    fi
    if [[ "${execution_phase[${debt_id}]}" != "${phase}" || "${execution_path[${debt_id}]}" != "${path}" ]]; then
        echo "${path}: execution debt ${debt_id} does not match its registry entry" >&2
        failure=1
    fi
    execution_seen["${debt_id}"]=1
done < "${todo_occurrences}"

for debt_id in "${!execution_phase[@]}"; do
    if [[ -z "${execution_seen[${debt_id}]+x}" ]]; then
        echo "execution debt ${debt_id} has no matching source marker" >&2
        failure=1
    fi
done

if [[ "${final_mode}" == true ]]; then
    if ((${#debt_kind[@]} != 0)); then
        echo "final architecture gate requires oversized Rust debt to be empty" >&2
        failure=1
    fi
    if ((${#execution_phase[@]} != 0)); then
        echo "final architecture gate requires execution debt to be empty" >&2
        failure=1
    fi
fi

if ((failure != 0)); then
    exit 1
fi

printf 'architecture_gate\trust_files=%s\tdependency_edges=%s\toversized_debts=%s\texecution_debts=%s\tmode=%s\n' \
    "${#rust_files[@]}" "$(wc -l < "${actual_dependencies}" | tr -d ' ')" \
    "${#debt_kind[@]}" "${#execution_phase[@]}" \
    "$([[ "${final_mode}" == true ]] && printf final || printf incremental)"
