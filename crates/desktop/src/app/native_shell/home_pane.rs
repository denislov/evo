use std::sync::Arc;

use coding_agent::api::embedding::CodingAgentResourceCommand;
use desktop::runtime::DesktopSessionCatalogEntry;
use desktop::shell::{SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Role, Styled as _, div, prelude::*, px,
    rgb,
};
use gpui_component::StyledExt as _;

use super::{
    desktop_controls::{DesktopActionRow, DesktopControlSize, DesktopRowState},
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

const MAX_HOME_SESSIONS: usize = 6;
const MAX_HOME_SKILLS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HomePaneEvent {
    OpenSession(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomePaneViewModel {
    pub(super) model: Arc<str>,
    pub(super) thinking: Arc<str>,
    pub(super) recent_sessions: Arc<[DesktopSessionCatalogEntry]>,
    pub(super) omitted_sessions: usize,
    pub(super) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(super) session_pending: bool,
    pub(super) catalog_pending: bool,
    pub(super) notice: Option<Arc<str>>,
}

pub(super) struct HomePane {
    view_model: Option<HomePaneViewModel>,
}

impl HomePane {
    pub(super) fn new() -> Self {
        Self { view_model: None }
    }

    pub(super) fn set_view_model(&mut self, view_model: HomePaneViewModel) {
        self.view_model = Some(view_model);
    }
}

impl EventEmitter<HomePaneEvent> for HomePane {}

impl Render for HomePane {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().size_full().into_any_element();
        };
        let theme = SemanticTheme::GEEK_DARK;
        let session_rows = view_model
            .recent_sessions
            .iter()
            .take(MAX_HOME_SESSIONS)
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let title = session.name.as_deref().unwrap_or(&session.session_id);
                let detail = session
                    .cwd
                    .as_deref()
                    .map(|cwd| truncate_label(cwd, 46))
                    .unwrap_or_else(|| truncate_label(&session.updated_at, 32));
                DesktopActionRow::new(
                    ("home-session", index),
                    truncate_label(title, 34),
                    format!("Open recent session {title}"),
                )
                .state(DesktopRowState {
                    selected: false,
                    disabled: view_model.session_pending,
                    focus_visible: false,
                })
                .size(DesktopControlSize::Critical)
                .detail(detail)
                .build(theme)
                .debug_selector(move || format!("desktop-home-session-{index}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(HomePaneEvent::OpenSession(target.clone()));
                }))
            })
            .collect::<Vec<_>>();
        let skill_rows = view_model
            .global_skills
            .iter()
            .take(MAX_HOME_SKILLS)
            .enumerate()
            .map(|(index, skill)| {
                div()
                    .id(("home-skill", index))
                    .px_token(DesignSpace::Md)
                    .py_token(DesignSpace::Sm)
                    .rounded_token(DesignRadius::Sm)
                    .border_1()
                    .border_color(rgb(theme.divider.value()))
                    .bg(rgb(theme.surface.value()))
                    .child(
                        div()
                            .text_token(DesignText::Body)
                            .child(format!("/{}", truncate_label(&skill.name, 28))),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.muted_text.value()))
                            .child(truncate_label(&skill.description, 72)),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .id("home-pane")
            .debug_selector(|| "desktop-home-pane".into())
            .role(Role::Main)
            .aria_label("New conversation home")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(theme.canvas.value()))
            .child(
                div()
                    .w_full()
                    .max_w(px(1_100.))
                    .mx_auto()
                    .px_token(DesignSpace::Xl)
                    .py_12()
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Xl)
                    .child(
                        div()
                            .child(
                                div()
                                    .text_3xl()
                                    .font_semibold()
                                    .child("What should we build?"),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("Describe a task below. Evo creates a session only when you submit."),
                            )
                            .when_some(view_model.notice.clone(), |header, notice| {
                                header.child(
                                    div()
                                        .mt_2()
                                        .text_token(DesignText::Metadata)
                                        .text_color(rgb(theme.warning.value()))
                                        .child(notice.to_string()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_token(DesignSpace::Md)
                            .text_token(DesignText::Metadata)
                            .child(format!("Model · {}", view_model.model))
                            .child(format!("Thinking · {}", view_model.thinking)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_token(DesignSpace::Xl)
                            .child(
                                div()
                                    .id("home-recent-sessions")
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .mb_3()
                                            .text_token(DesignText::Title)
                                            .child("RECENT SESSIONS"),
                                    )
                                    .children(session_rows)
                                    .when(
                                        view_model.recent_sessions.is_empty(),
                                        |column| {
                                            column.child(
                                                div()
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(if view_model.catalog_pending {
                                                        "Loading sessions…"
                                                    } else {
                                                        "No recent sessions yet."
                                                    }),
                                            )
                                        },
                                    )
                                    .when(view_model.omitted_sessions > 0, |column| {
                                        column.child(
                                            div()
                                                .mt_2()
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.muted_text.value()))
                                                .child(format!(
                                                    "{} older session(s) omitted",
                                                    view_model.omitted_sessions
                                                )),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id("home-global-skills")
                                    .min_w_64()
                                    .flex_1()
                                    .child(
                                        div()
                                            .mb_3()
                                            .text_token(DesignText::Title)
                                            .child("GLOBAL SKILLS"),
                                    )
                                    .children(skill_rows)
                                    .when(view_model.global_skills.is_empty(), |column| {
                                        column.child(
                                            div()
                                                .text_color(rgb(theme.muted_text.value()))
                                                .child("No global skills installed."),
                                        )
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
