#!/usr/bin/env bash
# Install one Evo release component from GitHub Releases on Linux x86_64.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/denislov/evo/main/scripts/install.sh | bash
#   ./scripts/install.sh --component desktop
#   ./scripts/install.sh --component cli --version 0.7.2 --install-dir ~/.local/bin
set -euo pipefail

readonly REPOSITORY="denislov/evo"
readonly RELEASES_URL="https://github.com/${REPOSITORY}/releases"

component="cli"
version=""
install_dir=""

usage() {
  cat <<'EOF'
Install Evo from GitHub Releases.

Options:
  --component <cli|desktop>  Component to install (default: cli)
  --version <version>        Release version, with or without a leading v (default: latest)
  --install-dir <directory>  Installation directory (default: ~/.local/bin)
  -h, --help                 Show this help
EOF
}

fail() {
  printf 'install: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
}

while (($# > 0)); do
  case "$1" in
    --component)
      (($# >= 2)) || fail "--component requires a value"
      component="$2"
      shift 2
      ;;
    --version)
      (($# >= 2)) || fail "--version requires a value"
      version="$2"
      shift 2
      ;;
    --install-dir)
      (($# >= 2)) || fail "--install-dir requires a value"
      install_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) fail "unknown option: $1" ;;
  esac
done

[[ "$(uname -s)" == "Linux" ]] || fail "this script supports Linux only"
[[ "$(uname -m)" == "x86_64" ]] || fail "this script supports x86_64 only"
case "$component" in
  cli|desktop) ;;
  *) fail "--component must be cli or desktop" ;;
esac

require_command curl
require_command sha256sum
require_command tar
require_command mktemp

if [[ -z "$install_dir" ]]; then
  install_dir="$HOME/.local/bin"
fi

if [[ -z "$version" ]]; then
  latest_url="$(curl --fail --silent --show-error --location --output /dev/null --write-out '%{url_effective}' "${RELEASES_URL}/latest")"
  version="${latest_url##*/}"
fi
version="${version#v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$ ]] || fail "invalid release version: $version"

archive="evo-${component}-${version}-x86_64-unknown-linux-gnu.tar.gz"
checksums="checksums.txt"
release_url="${RELEASES_URL}/download/v${version}"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/evo-install.XXXXXX")"
cleanup() {
  rm -rf "$temp_dir"
}
trap cleanup EXIT

curl --fail --silent --show-error --location "${release_url}/${checksums}" --output "${temp_dir}/${checksums}"
curl --fail --silent --show-error --location "${release_url}/${archive}" --output "${temp_dir}/${archive}"

(
  cd "$temp_dir"
  grep -F "  ${archive}" "$checksums" | sha256sum --check --status -
) || fail "SHA-256 verification failed for ${archive}"

tar --extract --gzip --file "${temp_dir}/${archive}" --directory "$temp_dir"
binary="$temp_dir/$component"
[[ -f "$binary" ]] || fail "release archive did not contain expected binary: $component"

mkdir -p "$install_dir"
target_name="$component"
if [[ "$component" == "cli" ]]; then
  target_name="coding-agent"
fi
install --mode 0755 "$binary" "${install_dir}/${target_name}"
printf 'Installed %s %s to %s\n' "$component" "$version" "${install_dir}/${target_name}"
printf 'Ensure %s is on PATH before running it.\n' "$install_dir"
