# Desktop performance baseline

This document records the repeatable headless and opt-in native release gates introduced for DESK-017. The native gate measures GPUI's real `draw + platform_window.draw/present` boundary at P95 and P99. It also pairs 50 simulated keystrokes dispatched through the focused production InputState with the first following GPUI post-render callback; this is a conservative internal render/present-submit upper bound, not a claim of click-to-photon latency.

## Run the gate

```bash
./scripts/desktop-perf-gate.sh
```

The gate runs ignored release-only tests so ordinary debug unit-test runs stay fast. Hydration/RSS fixtures run in a fresh process before the NativeShell replay, preventing the GPUI window fixture from preheating allocator pages and flattening the RSS curve. The content matrix separately measures bounded-preview sanitization and GPUI's real synchronous `TextViewState::markdown` parser. Each threshold is asserted by the test, and the complete output—including raw tab-separated `desktop_perf` rows—is written to `target/desktop-perf/latest.log` for local or CI comparison. Windows has an equivalent `scripts/desktop-perf-gate.ps1` entry point.

On a machine with an interactive X11 or Wayland display, run the native platform gate separately:

```bash
./scripts/desktop-native-perf-gate.sh
```

It builds the real release binary, opens a deterministic 1,300×900 NativeShell containing 10,000 transcript blocks, warms 20 frames, forces 200 further redraws, and exits automatically. GPUI's `ZED_MEASUREMENTS` hook brackets `Window::draw` and `Window::present`, including the platform renderer's draw/present call. Every fourth measured frame also dispatches one simulated `a` keystroke through the focused Composer InputState; the next post-render callback closes that input sample. The script requires exactly 200 frame and 50 paired input samples, calculates nearest-rank P95/P99, asserts frame time against 16.7/33 ms and the input upper-bound P95 against 50 ms, requires production Markdown completion samples, and measures production-process RSS before the window, after warmup, and after the replay. It writes `target/desktop-perf/native-latest.log`. It intentionally fails early when neither `DISPLAY` nor `WAYLAND_DISPLAY` is available instead of silently substituting the headless platform. Windows has an equivalent `scripts/desktop-native-perf-gate.ps1` entry point; physical input/display sampling is specified in `docs/desktop-external-performance.md`.

## Baseline environment

- Date: 2026-07-27
- OS: Linux 6.12.95+deb13-amd64 x86_64
- CPU: AMD Ryzen 7 7840HS, 8 cores / 16 threads
- Rust: rustc 1.96.0
- Cargo: cargo 1.96.0
- Profile: `release`
- Native display: X11, 2,880×1,800 at 120 Hz
- Native renderer: AMD Radeon 780M, Mesa 25.0.7, direct rendering

## Results

| Fixture | Measurement | Baseline |
|---|---:|---:|
| Empty conversation | retained blocks | 0 |
| 12,948,890-byte / 10,000-block transcript | hydration | 15,704 µs |
| Same transcript | hydration allocations | 30,015 allocations / 4,725,919 cumulative bytes |
| Same transcript | retained projection text and metadata | 13,108,890 bytes |
| Same transcript | Linux hydration RSS before / after / growth | 25,608,192 / 28,614,656 / 3,006,464 bytes |
| Same transcript, 500 visible-window preparations | P95 | 233 µs |
| Same transcript, 500 Composer edits | P95 | 1 µs |
| 10,000-block NativeShell, 200 forced uncached headless frames | CPU frame P95 | 1,594 µs |
| Same NativeShell, 200 keyboard-dispatched InputState changes | headless input roundtrip P95 | 3,027 µs |
| Same input replay | change handler → ComposerPane render P95 | 255 µs |
| Same NativeShell after projection construction | Linux window/component RSS before / after / growth | 27,193,344 / 50,913,280 / 23,719,936 bytes |
| Same 10,000-block fixture in a real X11 window, 200 post-warmup forced redraws | native GPU/present frame P95 | 3,339 µs |
| Same native replay | native GPU/present frame P99 | 3,954 µs |
| Same native replay | platform frame callback cadence P95 | 8,374 µs |
| Same native replay, 50 InputState keystrokes | dispatch → first post-render callback P95 / P99 | 8,340 / 16,712 µs |
| Same native replay | production RSS before window / after 20-frame warmup / after 200 frames | 32,931,840 / 149,442,560 / 149,479,424 bytes |
| Same native replay | production RSS startup / steady-state growth | 116,510,720 / 36,864 bytes |
| Five actually mounted production Markdown rows | parse→layout completion P95 | 157 µs |
| 1 / 100 / 1,000 / 10,000 short blocks | hydration | 21 / 69 / 275 / 2,744 µs |
| Same scale matrix | hydration allocations | 6 / 308 / 3,011 / 30,015 |
| Same scale matrix | cumulative allocated bytes | 586 / 36,987 / 304,999 / 4,725,919 bytes |
| Same scale matrix | Linux hydration RSS growth | 196,608 / 24,576 / 57,344 / 245,760 bytes |
| 256 KB Markdown, bounded to 79,888 bytes / 3,072 lines | bounded-preview sanitize / actual GPUI parser P95 | 439 / 80,954 µs |
| 648 KB Reasoning, bounded to 83,001 bytes | bounded-preview sanitize / actual GPUI parser P95 | 358 / 19,131 µs |
| 1 MB Bash output, bounded to 39,931 bytes | bounded-preview sanitize / actual GPUI parser P95 | 55 / 4,507 µs |
| 226 KB table, bounded to 57,913 bytes | bounded-preview sanitize / actual GPUI parser P95 | 378 / 6,156 µs |
| 336 KB code + CJK + Emoji, bounded to 39,931 bytes | bounded-preview sanitize / actual GPUI parser P95 | 54 / 4,458 µs |
| 10 / 50 / 200 streaming row revisions | per-event P95 | 22 / 5 / 5 µs |

The final-render preview retains at most 3,072 lines. A 4,096-line limit produced
a 106,512-byte Markdown fixture and intermittently crossed the 150 ms gate
(158,004 µs in the rejected run). The 3,072-line bound retains 79,888 bytes of
the same fixture and restores substantial parser margin while preserving the
full original text for Copy; the visible preview includes an explicit
truncation notice.

## Pre-optimization comparison

The repository's initial commit, `a39615f`, already contains the same 12,948,890-byte/10,000-block interaction fixture. Five release runs on the baseline machine provide a source-level pre-optimization comparison for hydration, visible-window preparation, and Composer state updates. A test-only counting allocator was backported without changing product code; the exact reproducible patch is `docs/desktop-pre-optimization-allocation.patch`.

| Metric | Initial commit, five-run median | Current implementation, five-run median | Interpretation |
|---|---:|---:|---|
| Hydration | 1,739 µs | 14,577 µs | Current stable identity, revision, cache, and retained-size metadata add work, but remain inside one 16.7 ms frame |
| Hydration allocation count | 30,015 | 30,015 | Still linear at approximately three allocations per block |
| Hydration cumulative allocated bytes | 3,939,583 B | 4,725,919 B | +786,336 B / +20.0%, covered by the 8 MiB release ceiling |
| 500 visible-window preparations | 181 µs P95 | 178 µs P95 | Effectively unchanged at this fixture scale |
| 500 Composer edits | 1 µs P95 | 1 µs P95 | Unchanged |

Raw initial-commit hydration samples were 1,739 / 1,594 / 1,801 / 3,313 / 1,615 µs; scroll-preparation P95 samples were 181 / 190 / 167 / 193 / 171 µs. Raw current hydration samples were 14,492 / 14,577 / 14,580 / 14,375 / 15,568 µs; scroll-preparation P95 samples were 177 / 176 / 178 / 188 / 191 µs. Allocation values and the 1 µs Composer P95 were stable across all five runs.

To reproduce the historical measurement from the current repository:

```bash
repository_root="$(pwd)"
prebaseline_dir="$(mktemp -d /tmp/evo-desktop-prebaseline.XXXXXX)"
git worktree add --detach "${prebaseline_dir}" a39615f
git -C "${prebaseline_dir}" apply \
  "${repository_root}/docs/desktop-pre-optimization-allocation.patch"
cargo test --manifest-path "${prebaseline_dir}/Cargo.toml" \
  -p desktop --lib --release \
  conversation::tests::desktop_release_ten_mib_interaction_baseline -- \
  --ignored --nocapture --test-threads=1
git worktree remove --force "${prebaseline_dir}"
```

The initial commit did not contain the later NativeShell headless/native frame replay, an RSS probe, or a committed `Cargo.lock`. Consequently this evidence does not invent a historical full-tree frame, process-RSS, or GPU/present number: it uses the pinned UI git revisions and a Rust-1.96-compatible dependency resolution, and reports only the metrics actually exercised by the original fixture. Those missing historical measurements cannot be reconstructed without backporting a materially different render harness into the old implementation.

Timing at this scale varies with CPU frequency, scheduler activity, and compiler changes. The enforced budgets are intentionally tied to user-visible limits rather than these exact baseline values:

- visible-window and incremental row preparation P95: no more than 16.7 ms;
- Composer edit P95: no more than 16.7 ms;
- forced full-tree headless CPU frame and real InputState roundtrip P95: no more than 16.7 ms;
- native GPUI draw + platform GPU/present frame P95: no more than 16.7 ms;
- native GPUI draw + platform GPU/present frame P99: no more than 33 ms;
- native InputState dispatch to first post-render callback P95: no more than 50 ms;
- 10,000-block NativeShell window/component-tree RSS growth on Linux: no more than 64 MiB;
- production native-process RSS after replay: no more than 256 MiB;
- production native-process RSS growth after 20-frame warmup: no more than 64 MiB across 200 frames;
- bounded-preview sanitization and actual GPUI final-content parser P95: no more than 150 ms each;
- actually mounted production-row Markdown parse-to-layout completion P95: no more than 150 ms;
- hydration: no more than four allocations per block plus fixed slack;
- 10 MiB fixture hydration: no more than 8 MiB cumulatively allocated, guarding against cloning the retained payload during projection;
- on Linux, macOS, and Windows, hydration RSS/working-set growth: no more than 64 MiB per fixture.

## What the gate covers

- transcript scales of 1, 100, 1,000, and 10,000 blocks;
- a retained transcript larger than 10 MiB;
- a real 10,000-block NativeShell/GPUI component tree with 200 forced uncached CPU frames and 200 keyboard-dispatched InputState changes;
- the same deterministic tree in an opt-in native window with 20 warmup, 200 measured GPU/present redraws, and 50 paired InputState dispatch-to-post-render samples;
- production-process RSS startup/steady-state boundaries and real mounted-row Markdown completion tracing in that native window;
- simulated incremental rates of 10, 50, and 200 row revisions per second;
- Markdown, Reasoning, Bash output, tables, fenced code, CJK, and Emoji;
- bounded-preview sanitization, the actual GPUI Markdown parser, hydration, hydration allocation pressure, supported-platform process resident memory, visible-window preparation, Composer state update, cache-retained bytes, and projection-retained bytes.

## Remaining instrumentation

The headless GPUI release replay constructs the 10,000-block projection before its window RSS baseline, then measures the additional NativeShell/component-tree footprint after the first completed render. It calls `Window::refresh()` before every timing sample, so entity caching cannot turn the measurement into an idle no-op. Input samples use `Window::dispatch_keystroke`; programmatic `InputState::set_value` is intentionally not used because the component suppresses `InputEvent::Change` for that API. After dispatch, the harness drains the real change subscription and explicitly refreshes the window because `VisualTestContext` deliberately has no platform `on_request_frame` callback. It then executes the real NativeShell render, layout, prepaint, CPU paint, InputState event, and child-entity notification paths. The test platform does not implement GPU submission or presentation; `headless_cpu_frame` and `headless_input_roundtrip` therefore must not be reported as GPU frame or click-to-photon latency.

The separate native replay uses the production binary rather than a GPUI test binary because only the production platform loop exposes GPUI's internal `frame duration` measurement. It records exact nearest-rank P95/P99 values for the locked GPUI draw/present boundary and an informational callback-cadence P95; cadence reflects display refresh scheduling and is not added to draw/present time. For input, GPUI documents `on_next_frame` as running directly after the current frame is rendered. The replay therefore dispatches a keystroke before the current draw and timestamps the next callback, proving that InputState handling and at least one changed frame completed inside the measured interval. The value deliberately includes callback scheduling delay and is treated as a conservative upper bound. It excludes the physical keyboard/OS queue before dispatch and display scanout after present, so DESK-017 remains open for externally observed click-to-photon latency. A dedicated production black/white replay now accepts physical Space events, emits input-received/post-render sample pairs, and has a fail-closed external CSV validator; the equipment and sampling contract is in `docs/desktop-external-performance.md`. DESK-018's production-window screenshot/golden coverage is documented in `docs/desktop-visual-goldens.md`.

The native rows above are the last qualifying reference run. A later validation attempt on 2026-07-27 was correctly rejected because the active X11 session throttled callbacks to approximately 1 Hz (`native_frame_cadence_p95_us=1,008,248`, draw/present P95 `1,000,185 µs`, input-to-post-render P95 `1,000,014 µs`). Those samples are environmental failure evidence, not a replacement baseline; the gate failed closed instead of publishing them as a 60/120 Hz result.

The test binary uses a test-only counting system allocator to report cumulative successful allocations around hydration. The release gate is single-threaded so unrelated tests cannot contaminate the deltas. The resident-memory probe itself is production-capable: Linux reads `VmRSS` from `/proc/self/status`, macOS queries `MACH_TASK_BASIC_INFO.resident_size`, and Windows queries `PROCESS_MEMORY_COUNTERS.WorkingSetSize`. Fixture construction is outside the hydration window. Resident memory is allocator- and scheduler-sensitive, so the gate uses regression ceilings rather than treating small deltas as exact retained-heap measurements. The native startup delta includes the platform window, graphics backend, font services, GPU resources, and NativeShell, so it is recorded separately from post-warmup steady growth. Unsupported platforms report `rss_supported=false`; their numeric RSS fields are zero sentinels and must not be interpreted as measurements. Linux now has headless and production-window samples; macOS and Windows have the same Bash/PowerShell gates but still require qualifying native-machine samples before DESK-017 can close.

The desktop adapter exposes opt-in `tracing` spans/events for `desktop.runtime.batch_wait`, `desktop.runtime.receive`, `desktop.runtime.batch_size`, `desktop.projection.apply`, `desktop.preview.sanitize`, `desktop.list.height_update`, `desktop.list.layout`, `desktop.render.prepare_rows`, `desktop.render`, `desktop.input.change`, and `desktop.input.to_render`. The input event measures the latest Composer change handler to the next ComposerPane render and is consumed exactly once. The release parser matrix directly constructs `TextViewState::markdown` with the same bounded content fixtures. Production row tracing avoids an externally owned Markdown state lifecycle: a transparent Element delegates to the normal keyed `TextView::markdown`, and when `EVO_DESKTOP_MARKDOWN_TRACE=1` it emits a completion only after the real request-layout call returns. The trace includes the stable session-scoped state key, phase, bytes, and conservative parse-to-layout duration; the default path does not allocate trace state or start a timer. The host application or benchmark harness still owns process-global tracing subscriber installation. These spans and gates provide CPU/render-preparation timing boundaries but do not mislabel internal callbacks as physical end-to-end latency.
