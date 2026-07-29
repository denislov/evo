#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${script_dir}/.." && pwd)"
cd "${repository_root}"

artifact_dir="${repository_root}/target/desktop-perf"
log_file="${artifact_dir}/click-to-photon-app-latest.log"
minimum_samples=50
drive_replay=0
drive_samples=60

usage() {
    cat >&2 <<'USAGE'
usage: desktop-click-to-photon.sh [--drive [SAMPLES]]

  (no flag)     Manual capture. Opens the black/white replay surface and waits
                for a human to press Space while an external photodiode records
                the matching photon timestamps. This is the only mode that
                produces real click-to-photon latency.
  --drive [N]   X11 smoke drive. Injects N Space presses (default 60, minimum
                50) into the replay window and then ends the replay by signal,
                so the harness can be verified without a human or a sensor. It
                only exercises the app-side input-received-to-post-render half;
                it does NOT measure photon latency and never replaces the
                manual run. It takes X input focus for the duration.
USAGE
}

while (( $# > 0 )); do
    case "$1" in
        --drive)
            drive_replay=1
            if [[ "${2:-}" =~ ^[0-9]+$ ]]; then
                drive_samples="$2"
                shift
            fi
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
    shift
done

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "click-to-photon replay requires DISPLAY or WAYLAND_DISPLAY" >&2
    exit 2
fi

if (( drive_replay )); then
    if [[ -z "${DISPLAY:-}" ]]; then
        echo "--drive requires an X11 DISPLAY; Wayland sessions have no xdotool injection path" >&2
        exit 2
    fi
    if ! command -v xdotool >/dev/null 2>&1; then
        echo "--drive requires xdotool" >&2
        exit 2
    fi
    if (( drive_samples < minimum_samples )); then
        echo "--drive must request at least ${minimum_samples} samples; got ${drive_samples}" >&2
        exit 2
    fi
fi

mkdir -p "${artifact_dir}"

# Resolve the replay window by process, not by title match alone: window titles
# are not unique and a stale window from an earlier run would silently absorb
# the injected keys.
find_replay_window() {
    local app_pid="$1"
    local candidate name
    for candidate in $(xdotool search --pid "${app_pid}" 2>/dev/null || true); do
        name="$(xdotool getwindowname "${candidate}" 2>/dev/null || true)"
        if [[ "${name}" == *click-to-photon* ]]; then
            printf '%s' "${candidate}"
            return 0
        fi
    done
    return 1
}

# The replay window must hold X input focus for every injected key. When an XIM
# input method is connected (XMODIFIERS=@im=..., e.g. fcitx5/ibus) GPUI forwards
# key events to the IM server instead of handling them itself, and the IM server
# drops events aimed at an unfocused input context. Keys sent to a background
# window are then discarded outright — not queued — so the replay logs nothing.
focus_replay_window() {
    local window_id="$1"
    xdotool windowactivate --sync "${window_id}" >/dev/null 2>&1 || return 1
    local active
    active="$(xdotool getactivewindow 2>/dev/null || echo 0)"
    (( active == window_id ))
}

drive_replay_window() {
    : >"${log_file}"
    env EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY=1 \
        "${repository_root}/target/release/desktop" >"${log_file}" 2>&1 &
    local app_pid=$!
    # shellcheck disable=SC2064
    trap "kill ${app_pid} 2>/dev/null || true" EXIT

    local window_id=""
    local deadline=$((SECONDS + 60))
    while (( SECONDS < deadline )); do
        if ! kill -0 "${app_pid}" 2>/dev/null; then
            echo "click-to-photon replay exited before its window appeared; see ${log_file}" >&2
            return 1
        fi
        if grep -q 'click_to_photon_run' "${log_file}" 2>/dev/null; then
            window_id="$(find_replay_window "${app_pid}" || true)"
            if [[ -n "${window_id}" ]]; then
                break
            fi
        fi
        sleep 0.2
    done
    if [[ -z "${window_id}" ]]; then
        echo "click-to-photon replay window never appeared within 60s" >&2
        return 1
    fi

    if ! focus_replay_window "${window_id}"; then
        echo "click-to-photon replay window could not take X input focus" >&2
        return 1
    fi
    echo "Driving ${drive_samples} Space presses into replay window ${window_id}."

    local sent=0
    local refocus_attempts=0
    local active
    while (( sent < drive_samples )); do
        if ! kill -0 "${app_pid}" 2>/dev/null; then
            echo "click-to-photon replay exited after ${sent} of ${drive_samples} driven samples" >&2
            break
        fi
        active="$(xdotool getactivewindow 2>/dev/null || echo 0)"
        if (( active != window_id )); then
            refocus_attempts=$((refocus_attempts + 1))
            if (( refocus_attempts > 10 )) || ! focus_replay_window "${window_id}"; then
                echo "click-to-photon replay lost X input focus after ${sent} samples;" \
                    "keep other windows from stealing focus while --drive runs" >&2
                return 1
            fi
            continue
        fi
        xdotool key --window "${window_id}" space || true
        sent=$((sent + 1))
        sleep 0.05
    done

    # Never inject Escape. The XIM path that carries injected keys replays them
    # against whatever holds focus, so an Escape aimed at the replay can land in
    # the terminal that launched this script once the replay window goes away.
    # The replay is our own child, so end it by signal instead; stdout is
    # line-buffered, and every sample is already on disk.
    sleep 0.5
    kill "${app_pid}" 2>/dev/null || true

    local exit_deadline=$((SECONDS + 15))
    while kill -0 "${app_pid}" 2>/dev/null && (( SECONDS < exit_deadline )); do
        sleep 0.2
    done
    if kill -0 "${app_pid}" 2>/dev/null; then
        echo "click-to-photon replay did not exit; killing it" >&2
        kill -9 "${app_pid}" 2>/dev/null || true
    fi
    wait "${app_pid}" 2>/dev/null || true
    trap - EXIT
}

cargo build -p desktop --release
if (( drive_replay )); then
    echo "Driving the replay without a sensor: this validates the harness and the app-side"
    echo "input-received-to-post-render half only, and is not a click-to-photon measurement."
    drive_replay_window
else
    echo "Press Space at least ${minimum_samples} times while the external sensor records matching sample IDs; press Escape only after the final post-render sample."
    echo "The external CSV must contain run_id,sample_id,latency_us and use the run ID printed by this replay."
    echo "Keep the replay window focused: keys delivered to an unfocused window are dropped."
    env EVO_DESKTOP_CLICK_TO_PHOTON_REPLAY=1 \
        "${repository_root}/target/release/desktop" 2>&1 | tee "${log_file}"
fi

run_summary="$(
    awk -F '\t' '
        $2 == "click_to_photon_post_render" && $3 ~ /^run=/ {
            runs[substr($3, 5)] = 1
        }
        END {
            for (run in runs) {
                count += 1
                run_id = run
            }
            print count + 0 "\t" run_id
        }
    ' "${log_file}"
)"
run_count="${run_summary%%$'\t'*}"
run_id="${run_summary#*$'\t'}"
if (( run_count != 1 )); then
    echo "click-to-photon replay must emit exactly one run ID; found ${run_count}" >&2
    exit 1
fi

sample_count="$(
    awk -F '\t' -v expected_run="${run_id}" '
        $2 == "click_to_photon_post_render" &&
        $3 == "run=" expected_run &&
        $4 ~ /^sample=[0-9]+$/ {
            samples[substr($4, 8)] = 1
        }
        END {
            for (sample in samples) {
                count += 1
            }
            print count + 0
        }
    ' "${log_file}"
)"
if (( sample_count < minimum_samples )); then
    echo "click-to-photon replay requires at least ${minimum_samples} post-render samples; found ${sample_count}" >&2
    exit 1
fi
echo "click-to-photon replay captured ${sample_count} unique post-render samples for run ${run_id} in ${log_file}"
