# Desktop performance baseline

This document records the repeatable headless and opt-in native release gates introduced for DESK-017. The native gate measures GPUI's real `draw + platform_window.draw/present` boundary at P95 and P99. It also pairs 50 simulated keystrokes dispatched through the focused production InputState with the first following GPUI post-render callback; this is a conservative internal render/present-submit upper bound, not a claim of click-to-photon latency.

## Run the gate

```bash
./scripts/desktop-perf-gate.sh
```

The gate runs ignored release-only tests so ordinary debug unit-test runs stay fast. Hydration/RSS fixtures run in a fresh process before the NativeShell replay, preventing the GPUI window fixture from preheating allocator pages and flattening the RSS curve. The content matrix separately measures bounded-preview sanitization and GPUI's real synchronous `TextViewState::markdown` parser. Each threshold is asserted by the test, and the complete output—including raw tab-separated `desktop_perf` rows—is written to `target/desktop-perf/latest.log` for local or CI comparison.

On a machine with an interactive X11 or Wayland display, run the native platform gate separately:

```bash
./scripts/desktop-native-perf-gate.sh
```

It builds the real release binary, opens a deterministic 1,300×900 NativeShell containing 10,000 transcript blocks, warms 20 frames, forces 200 further redraws, and exits automatically. GPUI's `ZED_MEASUREMENTS` hook brackets `Window::draw` and `Window::present`, including the platform renderer's draw/present call. Every fourth measured frame also dispatches one simulated `a` keystroke through the focused Composer InputState; the next post-render callback closes that input sample. The script requires exactly 200 frame and 50 paired input samples, calculates nearest-rank P95/P99, asserts frame time against 16.7/33 ms and the input upper-bound P95 against 50 ms, and writes `target/desktop-perf/native-latest.log`. It intentionally fails early when neither `DISPLAY` nor `WAYLAND_DISPLAY` is available instead of silently substituting the headless platform.

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
| 12,948,890-byte / 10,000-block transcript | hydration | 14,311 µs |
| Same transcript | hydration allocations | 30,015 allocations / 4,725,919 cumulative bytes |
| Same transcript | retained projection text and metadata | 13,108,890 bytes |
| Same transcript | Linux hydration RSS before / after / growth | 25,669,632 / 28,676,096 / 3,006,464 bytes |
| Same transcript, 500 visible-window preparations | P95 | 164 µs |
| Same transcript, 500 Composer edits | P95 | 1 µs |
| 10,000-block NativeShell, 200 forced uncached headless frames | CPU frame P95 | 1,573 µs |
| Same NativeShell, 200 keyboard-dispatched InputState changes | headless input roundtrip P95 | 2,765 µs |
| Same input replay | change handler → ComposerPane render P95 | 210 µs |
| Same NativeShell after projection construction | Linux window/component RSS before / after / growth | 27,516,928 / 51,294,208 / 23,777,280 bytes |
| Same 10,000-block fixture in a real X11 window, 200 post-warmup forced redraws | native GPU/present frame P95 | 3,181 µs |
| Same native replay | native GPU/present frame P99 | 3,444 µs |
| Same native replay | platform frame callback cadence P95 | 8,422 µs |
| Same native replay, 50 InputState keystrokes | dispatch → first post-render callback P95 / P99 | 9,094 / 9,369 µs |
| 1 / 100 / 1,000 / 10,000 short blocks | hydration | 19 / 61 / 536 / 2,140 µs |
| Same scale matrix | hydration allocations | 6 / 308 / 3,011 / 30,015 |
| Same scale matrix | cumulative allocated bytes | 586 / 36,987 / 304,999 / 4,725,919 bytes |
| Same scale matrix | Linux hydration RSS growth | 131,072 / 24,576 / 57,344 / 245,760 bytes |
| 256 KB Markdown | bounded-preview sanitize / actual GPUI parser P95 | 521 / 143,038 µs |
| 648 KB Reasoning | bounded-preview sanitize / actual GPUI parser P95 | 887 / 32,919 µs |
| 1 MB Bash output | bounded-preview sanitize / actual GPUI parser P95 | 88 / 9,404 µs |
| 226 KB table | bounded-preview sanitize / actual GPUI parser P95 | 333 / 6,119 µs |
| 336 KB code + CJK + Emoji | bounded-preview sanitize / actual GPUI parser P95 | 630 / 7,541 µs |
| 10 / 50 / 200 streaming row revisions | per-event P95 | 21 / 9 / 5 µs |

Timing at this scale varies with CPU frequency, scheduler activity, and compiler changes. The enforced budgets are intentionally tied to user-visible limits rather than these exact baseline values:

- visible-window and incremental row preparation P95: no more than 16.7 ms;
- Composer edit P95: no more than 16.7 ms;
- forced full-tree headless CPU frame and real InputState roundtrip P95: no more than 16.7 ms;
- native GPUI draw + platform GPU/present frame P95: no more than 16.7 ms;
- native GPUI draw + platform GPU/present frame P99: no more than 33 ms;
- native InputState dispatch to first post-render callback P95: no more than 50 ms;
- 10,000-block NativeShell window/component-tree RSS growth on Linux: no more than 64 MiB;
- bounded-preview sanitization and actual GPUI final-content parser P95: no more than 150 ms each;
- hydration: no more than four allocations per block plus fixed slack;
- 10 MiB fixture hydration: no more than 8 MiB cumulatively allocated, guarding against cloning the retained payload during projection;
- when `/proc/self/status` is available, hydration RSS growth: no more than 64 MiB per fixture.

## What the gate covers

- transcript scales of 1, 100, 1,000, and 10,000 blocks;
- a retained transcript larger than 10 MiB;
- a real 10,000-block NativeShell/GPUI component tree with 200 forced uncached CPU frames and 200 keyboard-dispatched InputState changes;
- the same deterministic tree in an opt-in native window with 20 warmup, 200 measured GPU/present redraws, and 50 paired InputState dispatch-to-post-render samples;
- simulated incremental rates of 10, 50, and 200 row revisions per second;
- Markdown, Reasoning, Bash output, tables, fenced code, CJK, and Emoji;
- bounded-preview sanitization, the actual GPUI Markdown parser, hydration, hydration allocation pressure, Linux process RSS, visible-window preparation, Composer state update, cache-retained bytes, and projection-retained bytes.

## Remaining instrumentation

The headless GPUI release replay constructs the 10,000-block projection before its window RSS baseline, then measures the additional NativeShell/component-tree footprint after the first completed render. It calls `Window::refresh()` before every timing sample, so entity caching cannot turn the measurement into an idle no-op. Input samples use `Window::dispatch_keystroke`; programmatic `InputState::set_value` is intentionally not used because the component suppresses `InputEvent::Change` for that API. After dispatch, the harness drains the real change subscription and explicitly refreshes the window because `VisualTestContext` deliberately has no platform `on_request_frame` callback. It then executes the real NativeShell render, layout, prepaint, CPU paint, InputState event, and child-entity notification paths. The test platform does not implement GPU submission or presentation; `headless_cpu_frame` and `headless_input_roundtrip` therefore must not be reported as GPU frame or click-to-photon latency.

The separate native replay uses the production binary rather than a GPUI test binary because only the production platform loop exposes GPUI's internal `frame duration` measurement. It records exact nearest-rank P95/P99 values for the locked GPUI draw/present boundary and an informational callback-cadence P95; cadence reflects display refresh scheduling and is not added to draw/present time. For input, GPUI documents `on_next_frame` as running directly after the current frame is rendered. The replay therefore dispatches a keystroke before the current draw and timestamps the next callback, proving that InputState handling and at least one changed frame completed inside the measured interval. The value deliberately includes callback scheduling delay and is treated as a conservative upper bound. It excludes the physical keyboard/OS queue before dispatch and display scanout after present, so DESK-017 remains open for externally observed click-to-photon latency and a cross-platform resident-memory curve. DESK-018's production-window screenshot/golden coverage is documented in `docs/desktop-visual-goldens.md`.

The native rows above are the last qualifying reference run. A later validation attempt on 2026-07-27 was correctly rejected because the active X11 session throttled callbacks to approximately 1 Hz (`native_frame_cadence_p95_us=1,008,248`, draw/present P95 `1,000,185 µs`, input-to-post-render P95 `1,000,014 µs`). Those samples are environmental failure evidence, not a replacement baseline; the gate failed closed instead of publishing them as a 60/120 Hz result.

The test binary uses a test-only counting system allocator to report cumulative successful allocations around hydration. The release gate is single-threaded so unrelated tests cannot contaminate the deltas. On Linux it additionally samples `VmRSS` immediately before and after hydration; fixture construction is outside that window. RSS is allocator- and scheduler-sensitive, so the gate uses a regression ceiling rather than treating small deltas as exact retained-heap measurements. Other platforms report `rss_supported=false`; their numeric RSS fields are zero sentinels and must not be interpreted as measurements.

The desktop adapter now exposes opt-in `tracing` spans/events for `desktop.runtime.batch_wait`, `desktop.runtime.receive`, `desktop.runtime.batch_size`, `desktop.projection.apply`, `desktop.preview.sanitize`, `desktop.list.height_update`, `desktop.list.layout`, `desktop.render.prepare_rows`, `desktop.render`, `desktop.input.change`, and `desktop.input.to_render`. The input event measures the latest Composer change handler to the next ComposerPane render and is consumed exactly once. The release parser matrix directly constructs `TextViewState::markdown` with the same bounded content fixtures, so parser completion is measured without mislabeling preview sanitization as parsing. The component does not expose a production per-row parser-completion hook; an attempted externally owned state lifecycle also regressed the 10k InputState→ComposerPane replay and was rejected. The host application or benchmark harness owns subscriber installation; the desktop library does not replace a process-global subscriber. These spans and gates provide CPU/render-preparation timing boundaries but do not claim GPU paint or end-to-end input latency.
