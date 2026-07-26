# Desktop performance baseline

This document records the repeatable CPU-side release gate introduced for DESK-017. It does not claim GPU frame, paint, end-to-end input latency, allocator, or resident-memory measurements; those still require an instrumented desktop harness.

## Run the gate

```bash
./scripts/desktop-perf-gate.sh
```

The gate runs ignored release-only tests so ordinary debug unit-test runs stay fast. Each threshold is asserted by the test, and the complete output—including raw tab-separated `desktop_perf` rows—is written to `target/desktop-perf/latest.log` for local or CI comparison.

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
| 12,948,890-byte / 10,000-block transcript | hydration | 13,441 µs |
| Same transcript, 500 visible-window preparations | P95 | 167 µs |
| Same transcript, 500 Composer edits | P95 | 1 µs |
| 1 / 100 / 1,000 / 10,000 short blocks | hydration | 24 / 48 / 305 / 2,260 µs |
| 256 KB Markdown | bounded parse P95 | 566 µs |
| 648 KB Reasoning | bounded parse P95 | 476 µs |
| 1 MB Bash output | bounded parse P95 | 148 µs |
| 226 KB table | bounded parse P95 | 511 µs |
| 336 KB code + CJK + Emoji | bounded parse P95 | 77 µs |
| 10 / 50 / 200 streaming row revisions | per-event P95 | 22 / 7 / 5 µs |

Timing at this scale varies with CPU frequency, scheduler activity, and compiler changes. The enforced budgets are intentionally tied to user-visible limits rather than these exact baseline values:

- visible-window and incremental row preparation P95: no more than 16.7 ms;
- Composer edit P95: no more than 16.7 ms;
- bounded final-content parse P95: no more than 150 ms.

## What the gate covers

- transcript scales of 1, 100, 1,000, and 10,000 blocks;
- a retained transcript larger than 10 MiB;
- simulated incremental rates of 10, 50, and 200 row revisions per second;
- Markdown, Reasoning, Bash output, tables, fenced code, CJK, and Emoji;
- bounded parse, hydration, visible-window preparation, Composer state update, cache-retained bytes, and projection-retained bytes.

## Remaining instrumentation

DESK-017 remains open until a real GPUI window replay records frame P95/P99, paint, end-to-end input latency, allocations, and resident memory. DESK-018 will own screenshot/golden and end-to-end interaction coverage, including wide/medium/narrow layouts and authorization/recovery/file-review flows.

The desktop adapter now exposes opt-in `tracing` spans/events for `desktop.runtime.batch_wait`, `desktop.runtime.receive`, `desktop.runtime.batch_size`, `desktop.projection.apply`, `desktop.preview.sanitize`, `desktop.list.height_update`, `desktop.list.layout`, `desktop.render.prepare_rows`, `desktop.render`, and `desktop.input.change`. The host application or benchmark harness owns subscriber installation; the desktop library does not replace a process-global subscriber. These spans provide CPU timing boundaries but do not claim GPU paint or end-to-end input latency.
