#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

mode="compare"
if [[ "${1:-}" == "--update" ]]; then
    mode="update"
    shift
fi
if [[ "$#" -ne 0 ]]; then
    echo "usage: $0 [--update]" >&2
    exit 2
fi

if [[ -z "${DISPLAY:-}" ]]; then
    echo "desktop visual golden gate requires an X11 DISPLAY" >&2
    exit 2
fi

for command_name in xwininfo xprop wmctrl gnome-screenshot convert compare identify; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "desktop visual golden gate requires ${command_name}" >&2
        exit 2
    fi
done

artifact_dir="${repository_root}/target/desktop-visual"
golden_dir="${repository_root}/crates/desktop/tests/goldens/native"
mkdir -p "${artifact_dir}"
if [[ "${mode}" == "update" ]]; then
    mkdir -p "${golden_dir}"
fi

current_pid=""
current_window_id=""

stop_current_replay() {
    if [[ -n "${current_window_id}" ]]; then
        wmctrl -ic "${current_window_id}" >/dev/null 2>&1 || true
    fi
    if [[ -n "${current_pid}" ]] && kill -0 "${current_pid}" >/dev/null 2>&1; then
        for _ in $(seq 1 20); do
            if ! kill -0 "${current_pid}" >/dev/null 2>&1; then
                break
            fi
            sleep 0.05
        done
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
    stop_current_replay
    exit "${status}"
}
trap cleanup_on_exit EXIT

find_window_id() {
    local title="$1"
    local needle="\"${title}\""
    xwininfo -root -tree 2>/dev/null \
        | awk -v needle="${needle}" 'index($0, needle) { print $1; exit }'
}

capture_layout() {
    local layout="$1"
    local title="evo-desktop-visual-${layout}"
    local current_png="${artifact_dir}/${layout}.png"
    local replay_log="${artifact_dir}/${layout}.log"
    local existing_window_id
    local active_window_id
    local client_width
    local client_height
    local actual_size
    local color_count

    existing_window_id="$(find_window_id "${title}")"
    if [[ -n "${existing_window_id}" ]]; then
        echo "refusing to capture stale window ${existing_window_id} titled ${title}" >&2
        exit 1
    fi

    env EVO_DESKTOP_NATIVE_VISUAL_REPLAY="${layout}" \
        "${repository_root}/target/release/desktop" >"${replay_log}" 2>&1 &
    current_pid=$!

    current_window_id=""
    for _ in $(seq 1 100); do
        current_window_id="$(find_window_id "${title}")"
        if [[ "${current_window_id}" =~ ^0x[0-9a-fA-F]+$ ]]; then
            break
        fi
        if ! kill -0 "${current_pid}" >/dev/null 2>&1; then
            echo "native visual replay exited before opening ${title}" >&2
            sed -n '1,120p' "${replay_log}" >&2
            exit 1
        fi
        sleep 0.1
    done
    if [[ ! "${current_window_id}" =~ ^0x[0-9a-fA-F]+$ ]]; then
        echo "timed out waiting for native visual replay ${title}" >&2
        exit 1
    fi

    if ! xwininfo -id "${current_window_id}" 2>/dev/null \
        | grep -Fq "\"${title}\""; then
        echo "resolved window ${current_window_id} did not retain expected title ${title}" >&2
        exit 1
    fi
    client_width="$(xwininfo -id "${current_window_id}" 2>/dev/null \
        | awk '/^[[:space:]]*Width:/ { print $2; exit }')"
    client_height="$(xwininfo -id "${current_window_id}" 2>/dev/null \
        | awk '/^[[:space:]]*Height:/ { print $2; exit }')"
    if [[ ! "${client_width}" =~ ^[0-9]+$ || ! "${client_height}" =~ ^[0-9]+$ ]]; then
        echo "could not resolve client bounds for ${current_window_id}" >&2
        exit 1
    fi

    # Let the initial native frame settle before focusing the capture window.
    sleep 1
    wmctrl -ia "${current_window_id}"
    sleep 0.2
    active_window_id="$(xprop -root _NET_ACTIVE_WINDOW | awk '{ print $NF }')"
    if [[ "${active_window_id,,}" != "${current_window_id,,}" ]]; then
        echo "refusing to capture active window ${active_window_id}; expected ${current_window_id}" >&2
        exit 1
    fi
    gnome-screenshot -w -f "${current_png}"
    active_window_id="$(xprop -root _NET_ACTIVE_WINDOW | awk '{ print $NF }')"
    if [[ "${active_window_id,,}" != "${current_window_id,,}" ]]; then
        rm -f "${current_png}"
        echo "active window changed during capture; discarded ${layout} image" >&2
        exit 1
    fi
    # GNOME Screenshot includes server-side title-bar chrome. Crop from the
    # bottom to the X11 client bounds so the golden contains only Evo pixels.
    convert "${current_png}" -gravity South \
        -crop "${client_width}x${client_height}+0+0" +repage "${current_png}"

    actual_size="$(identify -format '%wx%h' "${current_png}")"
    color_count="$(identify -format '%k' "${current_png}")"
    if [[ ! "${color_count}" =~ ^[0-9]+$ ]]; then
        echo "could not count colors in ${layout} capture" >&2
        exit 1
    fi
    if (( color_count < 256 )); then
        echo "${layout} capture has only ${color_count} colors; refusing a blank GPU readback" >&2
        exit 1
    fi
    stop_current_replay

    if [[ "${mode}" == "warmup" ]]; then
        printf 'desktop_visual\tlayout=%s\tsize=%s\trenderer=warm\n' \
            "${layout}" "${actual_size}"
        return
    fi

    if [[ "${mode}" == "update" ]]; then
        cp "${current_png}" "${golden_dir}/${layout}.png"
        printf 'desktop_visual\tlayout=%s\tsize=%s\tgolden=updated\n' \
            "${layout}" "${actual_size}"
        return
    fi

    local golden_png="${golden_dir}/${layout}.png"
    local metric
    local compare_status=0
    local golden_size
    local normalized_rmse
    local rmse_budget="0.015"
    if [[ ! -f "${golden_png}" ]]; then
        echo "missing ${golden_png}; run $0 --update after reviewing captures" >&2
        exit 1
    fi
    golden_size="$(identify -format '%wx%h' "${golden_png}")"
    if [[ "${actual_size}" != "${golden_size}" ]]; then
        echo "${layout} capture was ${actual_size}; golden is ${golden_size}" >&2
        echo "run the gate in the pinned X11 render environment" >&2
        exit 1
    fi
    if metric="$(compare -metric RMSE "${golden_png}" "${current_png}" null: 2>&1)"; then
        compare_status=0
    else
        compare_status=$?
    fi
    if [[ "${compare_status}" -gt 1 ]]; then
        echo "ImageMagick failed comparing ${layout}: ${metric}" >&2
        exit 1
    fi
    normalized_rmse="$(printf '%s\n' "${metric}" | sed -n 's/.*(\([^)]*\)).*/\1/p')"
    if [[ -z "${normalized_rmse}" ]]; then
        echo "could not parse normalized RMSE for ${layout}: ${metric}" >&2
        exit 1
    fi
    if ! awk -v value="${normalized_rmse}" -v budget="${rmse_budget}" \
        'BEGIN { exit !(value <= budget) }'; then
        echo "${layout} visual RMSE ${normalized_rmse} exceeded ${rmse_budget}" >&2
        exit 1
    fi
    printf 'desktop_visual\tlayout=%s\tsize=%s\trmse=%s\trmse_budget=%s\n' \
        "${layout}" "${actual_size}" "${normalized_rmse}" "${rmse_budget}"
}

cargo build -p desktop --release

# The first GPU window after a fresh release build can populate driver/font
# caches with measurably different edge antialiasing. Exercise and discard one
# exact app-window capture so compared images all use the warm render path.
requested_mode="${mode}"
mode="warmup"
capture_layout wide
mode="${requested_mode}"

capture_layout wide
capture_layout medium
capture_layout narrow
