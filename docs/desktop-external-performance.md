# Desktop external performance evidence

This guide covers the evidence that cannot be produced by GPUI's headless test
platform: production-window resident memory, platform draw/present timing, and
physical input-to-photon latency. It deliberately separates application timing
from physical presentation so an internal callback is never reported as a
display measurement.

## Native performance and memory

Use an otherwise idle machine, a release build, the normal production graphics
driver, and a display running at its normal fixed refresh rate. Disable remote
desktop frame throttling and power-saving modes before collecting a qualifying
sample.

Linux and macOS:

```bash
./scripts/desktop-perf-gate.sh
./scripts/desktop-native-perf-gate.sh
```

Windows PowerShell:

```powershell
pwsh -File scripts/desktop-perf-gate.ps1
pwsh -File scripts/desktop-native-perf-gate.ps1
```

The headless gate writes `target/desktop-perf/latest.log`. The native gate
writes `target/desktop-perf/native-latest.log`. A qualifying platform run must
contain all of the following rather than relying only on the process exit code:

- `platform` equals the host platform and every `rss_supported` field is `true`;
- 10 MiB hydration and the 10,000-row headless component tree each remain under
  their 64 MiB RSS/working-set growth ceilings;
- the production process remains below 256 MiB RSS after the native replay;
- production RSS growth from the end of the 20-frame warmup through the next
  200 measured frames remains below 64 MiB;
- native draw/present P95 is at most 16.7 ms and P99 at most 33 ms;
- InputState dispatch to the first post-render callback P95 is at most 50 ms;
- production Markdown parse-to-layout completion P95 is at most 150 ms.

The production RSS startup delta is recorded but is not compared to the 64 MiB
headless component-tree budget. It includes the platform window, graphics
backend, font services, GPU resources, and the NativeShell tree. The absolute
256 MiB ceiling constrains that fixed footprint, while the 64 MiB steady-state
ceiling catches continued growth after warmup.

Archive both logs together with the OS version, GPU and driver, total memory,
display server, display refresh rate, Rust version, and tested commit. A macOS
or Windows result only qualifies for DESK-017 when it was produced natively on
that operating system; a cross-compiled binary or Linux compatibility layer is
not equivalent evidence.

## Production Markdown completion tracing

Set `EVO_DESKTOP_MARKDOWN_TRACE=1` when a normal production session needs row
level evidence. The default path performs no trace-state lookup, timing, or
logging. When enabled, each actual settling/final Markdown state emits one row
after the real `TextView::markdown` request-layout call returns:

```text
desktop_trace<TAB>markdown_parse_complete<TAB>state_key=...<TAB>phase=final<TAB>bytes=...<TAB>markdown_parse_to_layout_us=...
```

The stable state key contains session-scoped row identity, settling/final phase,
and source revision. A virtualized row that is destroyed and later mounted and
parsed again produces another completion, which is intentional. The duration
is a conservative parse-to-layout upper bound because gpui-component performs
full-replace Markdown parsing synchronously inside `request_layout`.

`desktop-native-perf-gate.sh` and its PowerShell equivalent enable the trace,
require at least one real production-row completion, calculate nearest-rank
P95, and fail above 150 ms.

## Physical click-to-photon measurement

The click-to-photon replay is a dedicated production window. It begins black;
each non-repeated physical Space key event flips the whole window between black
and white. Escape exits. Every accepted input produces paired application log
rows for OS-event receipt and the first post-render callback.

Launch it on Linux or macOS with:

```bash
./scripts/desktop-click-to-photon.sh
```

Launch it on Windows with:

```powershell
pwsh -File scripts/desktop-click-to-photon.ps1
```

The application log is written to
`target/desktop-perf/click-to-photon-app-latest.log`.

A qualifying physical setup must measure from actual switch closure—not from
software event injection—to the first visible transition near the bottom of the
displayed surface. A high-speed camera may capture both a switch-mounted LED
and that display region, or an electrical keyboard trigger may be paired with a
photodiode. Measuring the lower display region conservatively includes most of
the display scanout. Record the display refresh rate and camera/sensor sampling
rate. Synthetic `dispatch_keystroke`, XTest, remote-desktop input, and the
application's post-render callback are useful harness checks but are not
click-to-photon evidence.

Discard at least 10 warmup transitions, then collect at least 50 alternating
Space transitions without key repeat. Export the retained measurements as UTF-8
CSV using the application sample IDs:

```csv
sample_id,latency_us
11,18340
12,20110
```

Validate and write the machine-readable result with:

```bash
python3 scripts/desktop-click-to-photon-report.py measurements.csv \
  --platform linux \
  --app-log target/desktop-perf/click-to-photon-app-latest.log \
  --refresh-hz 120
```

Use `macos` or `windows` for the corresponding native host. The validator fails
unless there are at least 50 unique positive samples, every external sample ID
has a matching application post-render row, and nearest-rank P95 is at most
50 ms. The result is written to
`target/desktop-perf/click-to-photon-<platform>.log` and must be archived with
the raw CSV, app log, equipment description, and host metadata.
