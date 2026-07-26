# Desktop visual golden gate

DESK-018 uses an opt-in production-window replay because the locked GPUI 0.2.2 `VisualTestContext` exposes layout bounds but not rendered pixels. The replay contains deterministic User, Tool, Diagnostic, Reasoning, final Markdown, fenced-code, Composer, status-bar, Sessions, and Inspector content.

## Run the gate

Run this from an interactive X11 desktop with GNOME Screenshot and ImageMagick installed:

```bash
./scripts/desktop-visual-golden.sh
```

The script builds the release binary once and captures these logical viewports:

| Key | Requested viewport | Expected responsive result |
|---|---:|---|
| `wide` | 1,300×900 | Sessions, Conversation, and Inspector visible |
| `medium` | 900×800 | Sessions and Conversation visible; Inspector hidden |
| `narrow` | 700×800 | Conversation visible; both side panels hidden |

HiDPI scaling and the desktop work area determine the PNG's physical pixel dimensions. The compositor capture is cropped to the X11 client bounds, excluding title-bar chrome so the golden contains only Evo-rendered pixels. The gate rejects low-color blank/GPU-readback images, then requires the current and committed images to have identical physical dimensions and enforces a normalized RMSE no greater than `0.015`. Current images and replay logs are retained under `target/desktop-visual/` for review.

## Capture safety

Each replay uses an exact unique title. Before capture, the script resolves that title to one X11 window ID, activates that ID, and verifies `_NET_ACTIVE_WINDOW` immediately before and after `gnome-screenshot -w`. It aborts if a stale matching window exists, if the target cannot be activated, or if focus changes during capture. It never requests a full-screen or root-window screenshot, and cleanup closes or terminates only the child replay process started by that invocation.

## Update reviewed goldens

When an intentional visual change is ready, generate candidate images and review all three layouts before committing:

```bash
./scripts/desktop-visual-golden.sh --update
./scripts/desktop-visual-golden.sh
```

`--update` replaces only `crates/desktop/tests/goldens/native/{wide,medium,narrow}.png`. Do not update goldens merely to make an unexplained regression pass. Because these captures include compositor-rendered font output, run the gate in the pinned X11 render environment recorded in `docs/desktop-performance-baseline.md`; other platforms should continue to run the headless component and interaction tests even when they cannot run this visual gate.
