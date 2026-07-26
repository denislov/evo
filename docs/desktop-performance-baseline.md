# Desktop performance baseline

This document records the repeatable CPU-side release gate introduced for DESK-017. It does not claim GPU frame, paint, end-to-end input latency, allocator, or resident-memory measurements; those still require an instrumented desktop harness.

## Run the gate

```bash
./scripts/desktop-perf-gate.sh
```

The gate runs ignored release-only tests so ordinary debug unit-test runs stay fast. Hydration/RSS fixtures run in a fresh process before the NativeShell replay, preventing the GPUI window fixture from preheating allocator pages and flattening the RSS curve. Each threshold is asserted by the test, and the complete output—including raw tab-separated `desktop_perf` rows—is written to `target/desktop-perf/latest.log` for local or CI comparison.

## Baseline environment

- Date: 2026-07-27
- OS: Linux 6.12.95+deb13-amd64 x86_64
- CPU: AMD Ryzen 7 7840HS, 8 cores / 16 threads
- Rust: rustc 1.96.0
- Cargo: cargo 1.96.0
- Profile: `release`

## Results

| Fixture | Measurement | Baseline |
|---|---:|---:|
| Empty conversation | retained blocks | 0 |
| 12,948,890-byte / 10,000-block transcript | hydration | 11,927 µs |
| Same transcript | hydration allocations | 30,015 allocations / 4,201,695 cumulative bytes |
| Same transcript | retained projection text and metadata | 13,108,890 bytes |
| Same transcript | Linux hydration RSS before / after / growth | 25,374,720 / 25,407,488 / 32,768 bytes |
| Same transcript, 500 visible-window preparations | P95 | 181 µs |
| Same transcript, 500 Composer edits | P95 | 1 µs |
| 10,000-block NativeShell, 200 forced uncached headless frames | CPU frame P95 | 1,368 µs |
| Same NativeShell, 200 real InputState changes | headless input roundtrip P95 | 1,299 µs |
| Same input replay | change handler → ComposerPane render P95 | 196 µs |
| Same NativeShell after projection construction | Linux window/component RSS before / after / growth | 25,235,456 / 50,163,712 / 24,928,256 bytes |
| 1 / 100 / 1,000 / 10,000 short blocks | hydration | 14 / 60 / 479 / 4,286 µs |
| Same scale matrix | hydration allocations | 6 / 308 / 3,011 / 30,015 |
| Same scale matrix | cumulative allocated bytes | 522 / 32,955 / 272,295 / 4,201,695 bytes |
| Same scale matrix | Linux hydration RSS growth | 131,072 / 28,672 / 0 / 77,824 bytes |
| 256 KB Markdown | bounded parse P95 | 622 µs |
| 648 KB Reasoning | bounded parse P95 | 1,021 µs |
| 1 MB Bash output | bounded parse P95 | 72 µs |
| 226 KB table | bounded parse P95 | 382 µs |
| 336 KB code + CJK + Emoji | bounded parse P95 | 74 µs |
| 10 / 50 / 200 streaming row revisions | per-event P95 | 14 / 4 / 4 µs |

Timing at this scale varies with CPU frequency, scheduler activity, and compiler changes. The enforced budgets are intentionally tied to user-visible limits rather than these exact baseline values:

- visible-window and incremental row preparation P95: no more than 16.7 ms;
- Composer edit P95: no more than 16.7 ms;
- forced full-tree headless CPU frame and real InputState roundtrip P95: no more than 16.7 ms;
- 10,000-block NativeShell window/component-tree RSS growth on Linux: no more than 64 MiB;
- bounded final-content parse P95: no more than 150 ms;
- hydration: no more than four allocations per block plus fixed slack;
- 10 MiB fixture hydration: no more than 8 MiB cumulatively allocated, guarding against cloning the retained payload during projection;
- when `/proc/self/status` is available, hydration RSS growth: no more than 64 MiB per fixture.

## What the gate covers

- transcript scales of 1, 100, 1,000, and 10,000 blocks;
- a retained transcript larger than 10 MiB;
- a real 10,000-block NativeShell/GPUI component tree with 200 forced uncached CPU frames and 200 InputState changes;
- simulated incremental rates of 10, 50, and 200 row revisions per second;
- Markdown, Reasoning, Bash output, tables, fenced code, CJK, and Emoji;
- bounded parse, hydration, hydration allocation pressure, Linux process RSS, visible-window preparation, Composer state update, cache-retained bytes, and projection-retained bytes.

## Remaining instrumentation

The GPUI release replay constructs the 10,000-block projection before its window RSS baseline, then measures the additional NativeShell/component-tree footprint after the first completed render. It calls `Window::refresh()` before every timing sample, so entity caching cannot turn the measurement into an idle no-op. `VisualTestContext` executes the real NativeShell render, layout, prepaint, CPU paint, InputState event, and child-entity notification paths. Its test platform deliberately does not implement platform `on_request_frame`, GPU submission, or presentation; `headless_cpu_frame` and `headless_input_roundtrip` therefore must not be reported as GPU frame or click-to-photon latency.

DESK-017 remains open until a native platform window replay records GPU/presentation frame P95/P99, click-to-photon input latency, and a cross-platform resident-memory curve for the rendered window. DESK-018 will own screenshot/golden and end-to-end interaction coverage, including wide/medium/narrow layouts and authorization/recovery/file-review flows.

The test binary uses a test-only counting system allocator to report cumulative successful allocations around hydration. The release gate is single-threaded so unrelated tests cannot contaminate the deltas. On Linux it additionally samples `VmRSS` immediately before and after hydration; fixture construction is outside that window. RSS is allocator- and scheduler-sensitive, so the gate uses a regression ceiling rather than treating small deltas as exact retained-heap measurements. Other platforms report `rss_supported=false`; their numeric RSS fields are zero sentinels and must not be interpreted as measurements.

The desktop adapter now exposes opt-in `tracing` spans/events for `desktop.runtime.batch_wait`, `desktop.runtime.receive`, `desktop.runtime.batch_size`, `desktop.projection.apply`, `desktop.preview.sanitize`, `desktop.list.height_update`, `desktop.list.layout`, `desktop.render.prepare_rows`, `desktop.render`, `desktop.input.change`, and `desktop.input.to_render`. The last event measures the latest Composer change handler to the next ComposerPane render and is consumed exactly once. The host application or benchmark harness owns subscriber installation; the desktop library does not replace a process-global subscriber. These spans provide CPU/render-preparation timing boundaries but do not claim GPU paint or end-to-end input latency.
