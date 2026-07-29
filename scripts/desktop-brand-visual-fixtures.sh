#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

if [[ -z "${DISPLAY:-}" ]]; then
    echo "desktop brand fixtures require an X11 DISPLAY" >&2
    exit 2
fi
for command_name in xwininfo xprop wmctrl gnome-screenshot convert identify sha256sum; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "desktop brand fixtures require ${command_name}" >&2
        exit 2
    fi
done

artifact_dir="${repository_root}/target/desktop-brand-fixtures"
report="${artifact_dir}/REPORT.md"
manifest="${artifact_dir}/manifest.sha256"
mkdir -p "${artifact_dir}"
find "${artifact_dir}" -maxdepth 1 -type f -delete

current_pid=""
current_window_id=""

find_window_id() {
    local title="$1"
    local needle="\"${title}\""
    xwininfo -root -tree 2>/dev/null \
        | awk -v needle="${needle}" 'index($0, needle) { print $1; exit }'
}

stop_fixture() {
    if [[ -n "${current_window_id}" ]]; then
        wmctrl -ic "${current_window_id}" >/dev/null 2>&1 || true
    fi
    if [[ -n "${current_pid}" ]] && kill -0 "${current_pid}" >/dev/null 2>&1; then
        kill "${current_pid}" >/dev/null 2>&1 || true
    fi
    if [[ -n "${current_pid}" ]]; then
        wait "${current_pid}" >/dev/null 2>&1 || true
    fi
    current_pid=""
    current_window_id=""
}

cleanup_on_exit() {
    local status=$?
    trap - EXIT
    stop_fixture
    exit "${status}"
}
trap cleanup_on_exit EXIT

capture_mode() {
    local mode="$1"
    local title="evo-brand-visual-${mode}"
    local output="${artifact_dir}/${mode}.png"
    local log="${artifact_dir}/${mode}.log"
    local client_width
    local client_height
    local active_window_id
    local color_count

    env EVO_DESKTOP_BRAND_VISUAL_REPLAY="${mode}" \
        "${repository_root}/target/release/desktop" >"${log}" 2>&1 &
    current_pid=$!
    current_window_id=""
    for _ in $(seq 1 100); do
        current_window_id="$(find_window_id "${title}")"
        if [[ "${current_window_id}" =~ ^0x[0-9a-fA-F]+$ ]]; then
            break
        fi
        if ! kill -0 "${current_pid}" >/dev/null 2>&1; then
            echo "brand fixture exited before opening ${title}" >&2
            sed -n '1,120p' "${log}" >&2
            exit 1
        fi
        sleep 0.1
    done
    if [[ ! "${current_window_id}" =~ ^0x[0-9a-fA-F]+$ ]]; then
        echo "timed out waiting for ${title}" >&2
        exit 1
    fi

    client_width="$(xwininfo -id "${current_window_id}" 2>/dev/null \
        | awk '/^[[:space:]]*Width:/ { print $2; exit }')"
    client_height="$(xwininfo -id "${current_window_id}" 2>/dev/null \
        | awk '/^[[:space:]]*Height:/ { print $2; exit }')"
    if [[ ! "${client_width}" =~ ^[0-9]+$ || ! "${client_height}" =~ ^[0-9]+$ ]]; then
        echo "could not resolve client bounds for ${title}" >&2
        exit 1
    fi

    sleep 1
    wmctrl -ia "${current_window_id}"
    sleep 0.2
    active_window_id="$(xprop -root _NET_ACTIVE_WINDOW | awk '{ print $NF }')"
    if [[ "${active_window_id,,}" != "${current_window_id,,}" ]]; then
        echo "refusing to capture active window ${active_window_id}; expected ${current_window_id}" >&2
        exit 1
    fi
    gnome-screenshot -w -f "${output}"
    convert "${output}" -gravity South \
        -crop "${client_width}x${client_height}+0+0" +repage "${output}"
    color_count="$(identify -format '%k' "${output}")"
    if [[ ! "${color_count}" =~ ^[0-9]+$ ]] || (( color_count < 64 )); then
        echo "${mode} fixture has only ${color_count} colors; refusing a blank GPU readback" >&2
        exit 1
    fi
    printf 'desktop_brand_fixture\tmode=%s\tsize=%s\tcolors=%s\n' \
        "${mode}" "$(identify -format '%wx%h' "${output}")" "${color_count}"
    stop_fixture
}

cargo build -p desktop --release
capture_mode dark
capture_mode light
capture_mode monochrome

(
    cd "${artifact_dir}"
    sha256sum dark.png light.png monochrome.png >"$(basename "${manifest}")"
)
{
    echo "# Evo Loop visual fixture review"
    echo
    echo "All captures use the production GPUI component and path-only SVG assets."
    echo
    echo "| Mode | Fixture | Contract sizes |"
    echo "| --- | --- | --- |"
    echo "| dark | dark.png | compact 16/24/32 px; wordmark 200/360 px |"
    echo "| light | light.png | compact 16/24/32 px; wordmark 200/360 px |"
    echo "| monochrome | monochrome.png | compact 16/24/32 px; wordmark 200/360 px |"
} >"${report}"

printf 'desktop_brand_fixture\treview=%s\tmanifest=%s\n' "${report}" "${manifest}"
