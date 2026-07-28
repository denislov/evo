#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

mode="compare"
review_note=""
case "${1:-}" in
    "") ;;
    --review)
        mode="review"
        shift
        ;;
    --update)
        mode="update"
        shift
        if [[ "${1:-}" != "--review-note" || -z "${2:-}" ]]; then
            echo "--update requires --review-note FILE" >&2
            exit 2
        fi
        review_note="$2"
        shift 2
        ;;
    *)
        echo "usage: $0 [--review | --update --review-note FILE]" >&2
        exit 2
        ;;
esac
if [[ "$#" -ne 0 ]]; then
    echo "usage: $0 [--review | --update --review-note FILE]" >&2
    exit 2
fi

artifact_dir="${repository_root}/target/desktop-visual"
golden_dir="${repository_root}/crates/desktop/tests/goldens/native"
review_dir="${artifact_dir}/review"
review_manifest="${review_dir}/manifest.sha256"
review_report="${review_dir}/REPORT.md"
golden_names=(
    wide
    medium
    narrow
    wide-idle
    medium-idle
    narrow-idle
    wide-authorization
    wide-reduced-motion
    wide-keyboard-focus
    wide-no-color
)
mkdir -p "${artifact_dir}"

install_reviewed_goldens() {
    if [[ ! -s "${review_note}" ]]; then
        echo "review note must exist and contain a visual change explanation: ${review_note}" >&2
        exit 2
    fi
    if [[ ! -s "${review_manifest}" || ! -s "${review_report}" ]]; then
        echo "missing reviewed captures; run $0 --review first" >&2
        exit 2
    fi
    if ! command -v sha256sum >/dev/null 2>&1; then
        echo "desktop visual golden update requires sha256sum" >&2
        exit 2
    fi
    for name in "${golden_names[@]}"; do
        if [[ ! -f "${review_dir}/${name}-before.png" \
            || ! -f "${review_dir}/${name}-after.png" \
            || ! -f "${review_dir}/${name}-diff.png" ]]; then
            echo "review is incomplete for ${name}; run $0 --review again" >&2
            exit 2
        fi
        if ! grep -Fq "  ${name}-after.png" "${review_manifest}"; then
            echo "review manifest does not authorize ${name}" >&2
            exit 2
        fi
    done
    if ! (cd "${review_dir}" && sha256sum --check "${review_manifest}"); then
        echo "reviewed captures changed after review; run $0 --review again" >&2
        exit 1
    fi
    mkdir -p "${golden_dir}"
    for name in "${golden_names[@]}"; do
        cp "${review_dir}/${name}-after.png" "${golden_dir}/${name}.png"
    done
    {
        echo "# Native visual golden review"
        echo
        cat "${review_note}"
        echo
        cat "${review_report}"
    } >"${golden_dir}/REVIEW.md"
    printf 'desktop_visual\tgoldens=updated\treview=%s\tnote=%s\n' \
        "${review_report}" "${review_note}"
}

if [[ "${mode}" == "update" ]]; then
    install_reviewed_goldens
    exit 0
fi

if [[ -z "${DISPLAY:-}" ]]; then
    echo "desktop visual golden gate requires an X11 DISPLAY" >&2
    exit 2
fi

for command_name in xwininfo xprop wmctrl gnome-screenshot convert compare identify sha256sum python3; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        echo "desktop visual golden gate requires ${command_name}" >&2
        exit 2
    fi
done

if [[ "${mode}" == "review" ]]; then
    mkdir -p "${review_dir}"
    find "${review_dir}" -maxdepth 1 -type f -delete
    : >"${review_manifest}"
    {
        echo "# Native visual before/after review"
        echo
        echo "Generated from deterministic native GPUI replays. Review the paired images and diff before installing them with \`--update --review-note FILE\`."
        echo
        echo "| Fixture | Size | Normalized RMSE | Before | After | Diff |"
        echo "| --- | ---: | ---: | --- | --- | --- |"
    } >"${review_report}"
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

park_pointer_outside_replay() {
    # Hover actions are intentional UI, but they must not make the baseline
    # depend on wherever the operator last left the pointer. XWarpPointer keeps
    # it on the root window outside the replay's fixed (40,40) origin.
    python3 - <<'PY'
import ctypes

x11 = ctypes.CDLL("libX11.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x11.XDefaultRootWindow.restype = ctypes.c_ulong
x11.XWarpPointer.argtypes = [
    ctypes.c_void_p,
    ctypes.c_ulong,
    ctypes.c_ulong,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_uint,
    ctypes.c_uint,
    ctypes.c_int,
    ctypes.c_int,
]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
display = x11.XOpenDisplay(None)
if not display:
    raise SystemExit("could not open X11 display to park the visual-golden pointer")
root = x11.XDefaultRootWindow(display)
x11.XWarpPointer(display, 0, root, 0, 0, 0, 0, 1, 1)
x11.XFlush(display)
x11.XCloseDisplay(display)
PY
}

evaluate_image() {
    local name="$1"
    local current_png="$2"
    local golden_png="${golden_dir}/${name}.png"
    local baseline_png="${golden_png}"
    local actual_size
    local golden_size
    local metric
    local compare_status=0
    local normalized_rmse
    local rmse_budget="0.015"

    if [[ ! -f "${golden_png}" ]]; then
        if [[ "${mode}" != "review" || ! -f "${golden_dir}/wide.png" ]]; then
            echo "missing ${golden_png}; run $0 --review to inspect a new baseline" >&2
            exit 1
        fi
        local base_layout="${name%%-*}"
        baseline_png="${golden_dir}/${base_layout}.png"
        if [[ ! -f "${baseline_png}" ]]; then
            echo "missing layout baseline ${baseline_png} for new fixture ${name}" >&2
            exit 1
        fi
        if [[ "${name}" == "wide-no-color" ]]; then
            baseline_png="${review_dir}/${name}-before.png"
            convert "${golden_dir}/wide.png" -colorspace Gray "${baseline_png}"
        fi
    fi
    actual_size="$(identify -format '%wx%h' "${current_png}")"
    golden_size="$(identify -format '%wx%h' "${baseline_png}")"
    if [[ "${actual_size}" != "${golden_size}" ]]; then
        echo "${name} capture was ${actual_size}; golden is ${golden_size}" >&2
        echo "run the gate in the pinned X11 render environment" >&2
        exit 1
    fi

    if [[ "${mode}" == "review" ]]; then
        if [[ "${baseline_png}" != "${review_dir}/${name}-before.png" ]]; then
            cp "${baseline_png}" "${review_dir}/${name}-before.png"
        fi
        cp "${current_png}" "${review_dir}/${name}-after.png"
        if metric="$(compare -metric RMSE "${baseline_png}" "${current_png}" \
            "${review_dir}/${name}-diff.png" 2>&1)"; then
            compare_status=0
        else
            compare_status=$?
        fi
    elif metric="$(compare -metric RMSE "${baseline_png}" "${current_png}" null: 2>&1)"; then
        compare_status=0
    else
        compare_status=$?
    fi
    if [[ "${compare_status}" -gt 1 ]]; then
        echo "ImageMagick failed comparing ${name}: ${metric}" >&2
        exit 1
    fi
    normalized_rmse="$(printf '%s\n' "${metric}" | sed -n 's/.*(\([^)]*\)).*/\1/p')"
    if [[ -z "${normalized_rmse}" ]]; then
        echo "could not parse normalized RMSE for ${name}: ${metric}" >&2
        exit 1
    fi

    if [[ "${mode}" == "review" ]]; then
        (cd "${review_dir}" && sha256sum "${name}-after.png") >>"${review_manifest}"
        printf '| `%s` | `%s` | `%s` | `%s-before.png` | `%s-after.png` | `%s-diff.png` |\n' \
            "${name}" "${actual_size}" "${normalized_rmse}" "${name}" "${name}" "${name}" \
            >>"${review_report}"
        printf 'desktop_visual\tfixture=%s\tsize=%s\trmse=%s\treview=generated\n' \
            "${name}" "${actual_size}" "${normalized_rmse}"
        return
    fi

    if ! awk -v value="${normalized_rmse}" -v budget="${rmse_budget}" \
        'BEGIN { exit !(value <= budget) }'; then
        echo "${name} visual RMSE ${normalized_rmse} exceeded ${rmse_budget}" >&2
        exit 1
    fi
    printf 'desktop_visual\tfixture=%s\tsize=%s\trmse=%s\trmse_budget=%s\n' \
        "${name}" "${actual_size}" "${normalized_rmse}" "${rmse_budget}"
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
    park_pointer_outside_replay
    sleep 0.1
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

    evaluate_image "${layout}" "${current_png}"
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
capture_layout wide-idle
capture_layout medium-idle
capture_layout narrow-idle
capture_layout wide-authorization
capture_layout wide-reduced-motion
capture_layout wide-keyboard-focus

# The gray derivative is the explicit no-color-state screenshot. Its golden
# ensures labels, markers, borders, and hierarchy remain legible without hue.
convert "${artifact_dir}/wide.png" -colorspace Gray "${artifact_dir}/wide-no-color.png"
evaluate_image wide-no-color "${artifact_dir}/wide-no-color.png"

if [[ "${mode}" == "review" ]]; then
    printf 'desktop_visual\treview=%s\tmanifest=%s\n' \
        "${review_report}" "${review_manifest}"
fi
