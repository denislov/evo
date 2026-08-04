use std::path::PathBuf;
use std::time::Instant;

use coding_agent::api::embedding::CodingAgentAuthSnapshot;
use coding_agent::api::settings::CodingAgentSettingsSnapshot;
use tui::api::input::{InputEvent, parse_key};
use tui::api::render::{RenderScheduler, Tui};
use tui::api::testing::{TerminalOp, VirtualTerminal};
use tui::api::theme::dark_theme;

use super::*;

fn test_tui() -> (Tui<VirtualTerminal>, usize) {
    let mut tui = Tui::new(VirtualTerminal::new(80, 24));
    let root = InteractiveRoot::new_with_theme_models_and_settings(
        PathBuf::from("."),
        "test-model".to_string(),
        "session".to_string(),
        dark_theme(),
        Vec::new(),
        CodingAgentSettingsSnapshot::default(),
        CodingAgentAuthSnapshot::default(),
    );
    let root_id = tui.add_child_with_id(Box::new(root));
    tui.set_focus(Some(root_id));
    (tui, root_id)
}

#[test]
fn terminal_progress_transitions_through_the_owned_terminal() {
    let mut tui = Tui::new(VirtualTerminal::new(80, 24));

    set_terminal_progress(&mut tui, true).unwrap();
    set_terminal_progress(&mut tui, false).unwrap();

    assert_eq!(
        tui.terminal().ops(),
        &[
            TerminalOp::SetProgress(true),
            TerminalOp::SetProgress(false)
        ]
    );
}

#[test]
fn runtime_tick_advances_spinner_and_schedules_elapsed_refresh() {
    let (mut tui, root_id) = test_tui();
    root_mut(&mut tui, root_id)
        .unwrap()
        .set_status(InteractiveStatus::Running);
    let before = root_ref(&tui, root_id).unwrap().spinner_frame;
    let mut scheduler = RenderScheduler::new(NORMAL_RENDER_INTERVAL);

    schedule_runtime_refresh(&mut tui, root_id, &mut scheduler);

    assert_eq!(root_ref(&tui, root_id).unwrap().spinner_frame, before + 1);
    assert!(scheduler.has_pending());
}

#[test]
fn coalesced_stream_updates_do_not_bypass_the_render_interval() {
    let mut scheduler = RenderScheduler::new(NORMAL_RENDER_INTERVAL);
    let now = Instant::now();
    schedule_render(&mut scheduler, RenderRequest::changed(true));
    assert!(scheduler.should_render_now(now));
    assert!(scheduler.mark_rendered(now));

    for _ in 0..512 {
        schedule_render(&mut scheduler, RenderRequest::changed(true));
    }

    assert!(!scheduler.should_render_now(now));
    assert_eq!(
        scheduler.next_render_at(now),
        Some(now + NORMAL_RENDER_INTERVAL)
    );
}

#[test]
fn transient_overlay_roles_keep_independent_geometry_and_capture_policy() {
    let assistance = transient_overlay_options(TransientOverlayRole::ComposerAssistance, 4);
    assert_eq!(assistance.anchor, OverlayAnchor::BottomLeft);
    assert_eq!(assistance.width, Some(SizeValue::Columns(72)));
    assert_eq!(assistance.margin.left, 0);
    assert_eq!(assistance.margin.right, 0);
    assert_eq!(assistance.margin.bottom, 4);
    assert!(assistance.non_capturing);

    let support = transient_overlay_options(TransientOverlayRole::SupportPrompt, 5);
    assert_eq!(support.anchor, OverlayAnchor::BottomLeft);
    assert_eq!(support.margin.left, 2);
    assert_eq!(support.margin.right, 2);
    assert!(support.non_capturing);

    let modal = transient_overlay_options(TransientOverlayRole::ModalDialog, 9);
    assert_eq!(modal.anchor, OverlayAnchor::Center);
    assert_eq!(modal.width, Some(SizeValue::Columns(72)));
    assert!(!modal.non_capturing);
    assert_eq!(modal.margin.bottom, 1);

    let context = transient_overlay_options(TransientOverlayRole::ContextRailDetail, 6);
    assert_eq!(context.anchor, OverlayAnchor::RightCenter);
    assert_eq!(context.width, Some(SizeValue::Columns(38)));
    assert_eq!(context.margin.bottom, 6);
    assert!(!context.non_capturing);

    let drawer = transient_overlay_options(TransientOverlayRole::ContextDrawerDetail, 6);
    assert_eq!(drawer.anchor, OverlayAnchor::RightCenter);
    assert_eq!(drawer.width, Some(SizeValue::Percent(40)));

    let page = transient_overlay_options(TransientOverlayRole::ContextPageDetail, 6);
    assert_eq!(page.anchor, OverlayAnchor::TopLeft);
    assert_eq!(page.width, Some(SizeValue::Percent(100)));
    assert_eq!(page.margin.bottom, 1);
}

#[test]
fn fullscreen_slash_assistance_stays_aligned_across_resizes() {
    fn visible_column(line: &str, needle: &str) -> usize {
        let byte = line.find(needle).expect("expected text in rendered line");
        tui::api::render::visible_width(&line[..byte])
    }

    let (mut tui, root_id) = test_tui();
    install_transient_overlays(&mut tui, root_id).unwrap();
    for key in ["/", "h", "e"] {
        tui.dispatch_input(&InputEvent::Key(parse_key(key).unwrap()));
    }

    for (width, height) in [(60, 18), (80, 24), (120, 32), (160, 40)] {
        tui.terminal_mut().resize(width, height);
        sync_transient_overlays(&mut tui, root_id).unwrap();
        tui.render_once().unwrap();
        let lines = tui.rendered_lines();
        let assistance = lines
            .iter()
            .find(|line| line.contains("/help") && line.contains("Show help"))
            .unwrap_or_else(|| panic!("slash assistance missing at {width}x{height}"));
        let composer = lines
            .iter()
            .find(|line| line.contains("> /he"))
            .unwrap_or_else(|| panic!("composer missing at {width}x{height}"));
        assert_eq!(
            visible_column(assistance, "/help"),
            visible_column(composer, "/"),
            "slash assistance must remain attached after resize at {width}x{height}:\n{}",
            lines.join("\n")
        );
    }
}

#[test]
fn fullscreen_file_assistance_is_above_and_aligned_with_the_composer() {
    fn visible_column(line: &str, needle: &str) -> usize {
        let byte = line.find(needle).expect("expected text in rendered line");
        tui::api::render::visible_width(&line[..byte])
    }

    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("fixture.rs"), "fn main() {}\n").unwrap();
    let mut tui = Tui::new(VirtualTerminal::new(80, 24));
    let root = InteractiveRoot::new_with_theme_models_and_settings(
        cwd.path().to_path_buf(),
        "test-model".to_string(),
        "session".to_string(),
        dark_theme(),
        Vec::new(),
        CodingAgentSettingsSnapshot::default(),
        CodingAgentAuthSnapshot::default(),
    );
    let root_id = tui.add_child_with_id(Box::new(root));
    tui.set_focus(Some(root_id));
    install_transient_overlays(&mut tui, root_id).unwrap();
    for key in ["@", "f"] {
        tui.dispatch_input(&InputEvent::Key(parse_key(key).unwrap()));
    }
    sync_transient_overlays(&mut tui, root_id).unwrap();
    tui.render_once().unwrap();

    let lines = tui.rendered_lines();
    let assistance = lines
        .iter()
        .find(|line| line.contains("fixture.rs"))
        .unwrap_or_else(|| panic!("file assistance missing: {lines:#?}"));
    let composer = lines
        .iter()
        .find(|line| line.contains("> @f"))
        .unwrap_or_else(|| panic!("composer missing: {lines:#?}"));
    assert_eq!(
        visible_column(assistance, "fixture.rs"),
        visible_column(composer, "@f")
    );
    let assistance_row = lines
        .iter()
        .position(|line| line.contains("fixture.rs"))
        .unwrap();
    let composer_row = lines.iter().position(|line| line.contains("> @f")).unwrap();
    assert!(assistance_row < composer_row, "{lines:#?}");
}
