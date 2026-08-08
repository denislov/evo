#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "${script_dir}/.." && pwd)"
exceptions_file="${script_dir}/licenses/metadata-exceptions.tsv"

cd -- "${repository_root}"

for command_name in awk cargo diff jq sed sort tr wc; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "license audit requires ${command_name}" >&2
        exit 2
    fi
done

for required_file in \
    Cargo.lock \
    THIRD_PARTY_NOTICES.md \
    LICENSES/LicenseRef-Proprietary.txt \
    docs/refactor/provenance/README.md \
    docs/refactor/provenance/gpui-component.md \
    docs/refactor/provenance/grok-build.md \
    "${exceptions_file}" \
    patches/gpui-component/0001-text-seed-the-background-parse-accumulator-from-the-.patch
do
    if [[ ! -f "${required_file}" ]]; then
        echo "license audit is missing ${required_file}" >&2
        exit 1
    fi
done

metadata_file="$(mktemp)"
actual_missing="$(mktemp)"
expected_missing="$(mktemp)"
trap 'rm -f "${metadata_file}" "${actual_missing}" "${expected_missing}"' EXIT

cargo metadata --format-version 1 > "${metadata_file}"

jq -r '
    .packages[]
    | select(.license == null or .license == "")
    | [.name, (.source // "path")]
    | @tsv
' "${metadata_file}" | sort -u > "${actual_missing}"

awk -F '\t' 'NF && $1 !~ /^#/ { print $1 "\t" $2 }' "${exceptions_file}" \
    | sort -u > "${expected_missing}"

if ! diff -u "${expected_missing}" "${actual_missing}"; then
    echo "Cargo license metadata exceptions differ from the reviewed allowlist" >&2
    exit 1
fi

workspace_license_mismatch="$(jq -r '
    .workspace_members as $members
    | .packages[]
    | select(.id as $id | $members | index($id))
    | select(.license != "LicenseRef-Proprietary")
    | "\(.name)\t\(.license // "MISSING")"
' "${metadata_file}")"
if [[ -n "${workspace_license_mismatch}" ]]; then
    echo "workspace package license metadata mismatch:" >&2
    echo "${workspace_license_mismatch}" >&2
    exit 1
fi

gpl_packages="$(jq -r '
    .packages[]
    | select((.license // "") | test("GPL"))
    | [.name, .version, .license, (.source // "path")]
    | @tsv
' "${metadata_file}" | sort -u)"

printf 'license_audit\tpackages=%s\tmissing_metadata_exceptions=%s\tgpl_records=%s\n' \
    "$(jq '.packages | length' "${metadata_file}")" \
    "$(wc -l < "${actual_missing}" | tr -d ' ')" \
    "$(printf '%s\n' "${gpl_packages}" | sed '/^$/d' | wc -l | tr -d ' ')"

if [[ -n "${gpl_packages}" ]]; then
    echo "reviewed license expressions containing GPL/LGPL (see THIRD_PARTY_NOTICES.md):"
    echo "${gpl_packages}"
fi
