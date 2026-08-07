//! Native frame replay instrumentation and click-to-photon rendering.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, Bounds, Context, FocusHandle, KeyDownEvent, Keystroke, Render, Window, WindowBounds,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use gpui_component::Root;

use super::{INPUT_SAMPLE_FRAMES, INPUT_SAMPLE_STRIDE, SAMPLE_FRAMES, WARMUP_FRAMES};

pub(in crate::app::devtools) struct NativeFrameReplay {
    callbacks: usize,
    last_callback_at: Option<Instant>,
    cadence_samples: Vec<u128>,
    pending_input_at: Option<Instant>,
    input_dispatches: usize,
    input_post_render_samples: Vec<u128>,
    resident_before: Option<u64>,
    resident_after_warmup: Option<u64>,
}

pub(in crate::app::devtools) struct ClickToPhotonReplay {
    focus_handle: FocusHandle,
    run_id: String,
    bright: bool,
    samples: u64,
}

impl ClickToPhotonReplay {
    pub(in crate::app::devtools) fn new(cx: &mut Context<Self>) -> Self {
        let run_id = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        println!("desktop_trace\tclick_to_photon_run\trun={run_id}");
        Self {
            focus_handle: cx.focus_handle(),
            run_id,
            bright: false,
            samples: 0,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.quit();
            return;
        }
        if event.is_held || event.keystroke.key != "space" {
            return;
        }

        self.bright = !self.bright;
        self.samples = self.samples.saturating_add(1);
        let sample = self.samples;
        let bright = self.bright;
        let run_id = self.run_id.clone();
        let received_at = Instant::now();
        println!(
            "desktop_trace\tclick_to_photon_input_received\trun={run_id}\tsample={sample}\tbright={bright}"
        );
        window.on_next_frame(move |_, _| {
            println!(
                "desktop_trace\tclick_to_photon_post_render\trun={run_id}\tsample={sample}\t\
                 bright={bright}\tinput_received_to_post_render_us={}",
                received_at.elapsed().as_micros()
            );
        });
        cx.notify();
    }
}

impl Render for ClickToPhotonReplay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
            .id("desktop-click-to-photon-surface")
            .track_focus(&self.focus_handle)
            .key_context("DesktopClickToPhotonReplay")
            .capture_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(rgb(if self.bright { 0xffffff } else { 0x000000 }))
    }
}

impl NativeFrameReplay {
    pub(in crate::app::devtools) fn new() -> Self {
        Self {
            callbacks: 0,
            last_callback_at: None,
            cadence_samples: Vec::with_capacity(SAMPLE_FRAMES),
            pending_input_at: None,
            input_dispatches: 0,
            input_post_render_samples: Vec::with_capacity(INPUT_SAMPLE_FRAMES),
            resident_before: crate::resident_memory::resident_bytes(),
            resident_after_warmup: None,
        }
    }

    pub(in crate::app::devtools) fn observe_frame_callback(&mut self, now: Instant) {
        // The input is dispatched from the previous frame callback, before
        // that frame draws. GPUI runs this callback after that frame renders,
        // so the interval is a conservative app/render/present-submit upper
        // bound. It deliberately does not claim display click-to-photon time.
        if let Some(dispatched_at) = self.pending_input_at.take() {
            self.input_post_render_samples
                .push(now.saturating_duration_since(dispatched_at).as_micros());
        }
        if let Some(previous) = self.last_callback_at
            && self.callbacks >= WARMUP_FRAMES
            && self.cadence_samples.len() < SAMPLE_FRAMES
        {
            self.cadence_samples
                .push(now.saturating_duration_since(previous).as_micros());
        }
        self.last_callback_at = Some(now);
        self.callbacks += 1;
        if self.callbacks == WARMUP_FRAMES {
            self.resident_after_warmup = crate::resident_memory::resident_bytes();
        }
    }

    pub(in crate::app::devtools) fn should_dispatch_input(&self) -> bool {
        self.pending_input_at.is_none()
            && self.input_dispatches < INPUT_SAMPLE_FRAMES
            && !self.cadence_samples.is_empty()
            && self
                .cadence_samples
                .len()
                .is_multiple_of(INPUT_SAMPLE_STRIDE)
    }

    pub(in crate::app::devtools) fn mark_input_dispatched(&mut self, now: Instant) {
        self.pending_input_at = Some(now);
        self.input_dispatches += 1;
    }

    pub(in crate::app::devtools) fn complete(&self) -> bool {
        self.cadence_samples.len() >= SAMPLE_FRAMES
            && self.input_post_render_samples.len() >= INPUT_SAMPLE_FRAMES
            && self.pending_input_at.is_none()
    }
}

pub(in crate::app::devtools) fn open_click_to_photon(cx: &mut App) -> Result<(), String> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(40.), px(40.)),
            size: size(px(1_000.), px(700.)),
        })),
        window_min_size: Some(size(px(640.), px(480.))),
        app_id: Some("evo.desktop.click-to-photon".into()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        window.set_window_title("evo · click-to-photon measurement · Space toggles · Esc exits");
        let replay = cx.new(ClickToPhotonReplay::new);
        let focus_handle = replay.read(cx).focus_handle.clone();
        focus_handle.focus(window, cx);
        cx.new(|cx| Root::new(replay, window, cx))
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(in crate::app::devtools) fn schedule_frame(
    window: &mut Window,
    replay: Rc<RefCell<NativeFrameReplay>>,
) {
    window.on_next_frame(move |window, cx| {
        let now = Instant::now();
        let mut replay_ref = replay.borrow_mut();
        replay_ref.observe_frame_callback(now);
        let dispatch_input = replay_ref.should_dispatch_input();
        let complete = replay_ref.complete();
        drop(replay_ref);

        if complete {
            let mut replay_ref = replay.borrow_mut();
            let cadence_p95_micros = percentile(&mut replay_ref.cadence_samples, 95);
            let input_post_render_p95_micros =
                percentile(&mut replay_ref.input_post_render_samples, 95);
            let input_post_render_p99_micros =
                percentile(&mut replay_ref.input_post_render_samples, 99);
            let resident_after = crate::resident_memory::resident_bytes();
            let resident_startup_growth =
                match (replay_ref.resident_before, replay_ref.resident_after_warmup) {
                    (Some(before), Some(after)) => Some(after.saturating_sub(before)),
                    _ => None,
                };
            let resident_steady_growth = match (replay_ref.resident_after_warmup, resident_after) {
                (Some(before), Some(after)) => Some(after.saturating_sub(before)),
                _ => None,
            };
            println!(
                "desktop_perf\tplatform={}\tnative_presented_frames={}\t\
                 native_frame_cadence_p95_us={cadence_p95_micros}\t\
                 native_input_samples={}\t\
                 native_input_dispatch_to_post_render_p95_us={input_post_render_p95_micros}\t\
                 native_input_dispatch_to_post_render_p99_us={input_post_render_p99_micros}\t\
                 native_rss_supported={}\tnative_rss_before_bytes={}\t\
                 native_rss_after_warmup_bytes={}\tnative_rss_after_bytes={}\t\
                 native_rss_startup_growth_bytes={}\tnative_rss_steady_growth_bytes={}",
                std::env::consts::OS,
                replay_ref.cadence_samples.len(),
                replay_ref.input_post_render_samples.len(),
                replay_ref.resident_before.is_some()
                    && replay_ref.resident_after_warmup.is_some()
                    && resident_after.is_some(),
                replay_ref.resident_before.unwrap_or_default(),
                replay_ref.resident_after_warmup.unwrap_or_default(),
                resident_after.unwrap_or_default(),
                resident_startup_growth.unwrap_or_default(),
                resident_steady_growth.unwrap_or_default()
            );
            cx.quit();
        } else {
            if dispatch_input {
                let dispatched_at = Instant::now();
                let keystroke =
                    Keystroke::parse("a").expect("the native replay input keystroke remains valid");
                if !window.dispatch_keystroke(keystroke, cx) {
                    eprintln!(
                        "desktop native performance replay could not dispatch composer input"
                    );
                    cx.quit();
                    return;
                }
                replay.borrow_mut().mark_input_dispatched(dispatched_at);
            }
            schedule_frame(window, Rc::clone(&replay));
            window.refresh();
        }
    });
    window.refresh();
}

pub(in crate::app::devtools) fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    assert!(
        !samples.is_empty(),
        "percentile requires at least one sample"
    );
    assert!(
        (1..=100).contains(&percentile),
        "percentile must be between 1 and 100"
    );
    samples.sort_unstable();
    let index = (samples.len() * percentile).div_ceil(100).saturating_sub(1);
    samples[index]
}
