use coding_agent::api::embedding::CodingAgentResourceCommand;
use desktop::ui::shell::{SemanticTheme, truncate_label};
use gpui::{IntoElement, ParentElement as _, Render, Role, Styled as _, div, prelude::*, px, rgb};
use std::sync::Arc;

use crate::ui::components::style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SkillsPaneViewModel {
    pub(crate) skills: Arc<[CodingAgentResourceCommand]>,
}

pub(crate) fn view_model(skills: &Arc<[CodingAgentResourceCommand]>) -> SkillsPaneViewModel {
    SkillsPaneViewModel {
        skills: Arc::clone(skills),
    }
}

pub(crate) struct SkillsPane {
    view_model: SkillsPaneViewModel,
}

impl SkillsPane {
    pub(crate) fn new() -> Self {
        Self {
            view_model: SkillsPaneViewModel::default(),
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: SkillsPaneViewModel) {
        self.view_model = view_model;
    }
}

impl Render for SkillsPane {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = SemanticTheme::current(cx);
        let skill_count = self.view_model.skills.len();
        let skill_rows = self
            .view_model
            .skills
            .iter()
            .enumerate()
            .map(|(index, skill)| {
                div()
                    .id(("skill-row", index))
                    .debug_selector(move || format!("desktop-skill-row-{index}"))
                    .role(Role::ListItem)
                    .w_full()
                    .px_token(DesignSpace::Lg)
                    .py_token(DesignSpace::Md)
                    .rounded_token(DesignRadius::Md)
                    .border_1()
                    .border_color(rgb(theme.divider.value()))
                    .bg(rgb(theme.surface.value()))
                    .child(
                        div()
                            .text_token(DesignText::Title)
                            .child(format!("/{}", truncate_label(&skill.name, 48))),
                    )
                    .child(
                        div()
                            .mt_token(DesignSpace::Xs)
                            .text_token(DesignText::Body)
                            .text_color(rgb(theme.muted_text.value()))
                            .child(truncate_label(&skill.description, 160)),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .id("skills-pane")
            .debug_selector(|| "desktop-skills-pane".into())
            .role(Role::Region)
            .aria_label("Global skills")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(theme.canvas.value()))
            .child(
                div()
                    .w_full()
                    .max_w(px(900.))
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
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Skills"),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child(format!(
                                        "{skill_count} global skill{} available to every project.",
                                        if skill_count == 1 { "" } else { "s" }
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("skills-list")
                            .debug_selector(|| "desktop-skills-list".into())
                            .role(Role::List)
                            .aria_label("Installed global skills")
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Md)
                            .children(skill_rows)
                            .when(self.view_model.skills.is_empty(), |list| {
                                list.child(
                                    div()
                                        .p_token(DesignSpace::Lg)
                                        .rounded_token(DesignRadius::Md)
                                        .border_1()
                                        .border_color(rgb(theme.divider.value()))
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child("No global skills installed."),
                                )
                            }),
                    ),
            )
    }
}
