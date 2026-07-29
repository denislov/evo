use desktop::shell::SemanticTheme;
use gpui::{IntoElement, ParentElement as _, Render, Role, Styled as _, div, prelude::*, px, rgb};

use super::desktop_style::{DesignSpace, DesktopStyledExt as _};

pub(super) struct HomePane;

impl HomePane {
    pub(super) fn new() -> Self {
        Self
    }
}

impl Render for HomePane {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = SemanticTheme::GEEK_DARK;

        div()
            .id("home-pane")
            .debug_selector(|| "desktop-home-pane".into())
            .role(Role::Region)
            .aria_label("New conversation home")
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .bg(rgb(theme.canvas.value()))
            .child(
                div()
                    .w_full()
                    .max_w(px(900.))
                    .min_h(px(260.))
                    .mx_auto()
                    .px_token(DesignSpace::Xl)
                    .py_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .child(
                        div()
                            .debug_selector(|| "desktop-evo-wordmark".into())
                            .text_size(px(112.))
                            .line_height(px(112.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.text.value()))
                            .child("evo"),
                    )
                    .child(
                        div()
                            .mt_token(DesignSpace::Lg)
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Software evolves. Your agent should too."),
                    )
                    .child(
                        div()
                            .mt_token(DesignSpace::Md)
                            .max_w(px(680.))
                            .text_color(rgb(theme.muted_text.value()))
                            .child(
                                "Describe what you want to build, fix, or understand. Evo will plan, act, and adapt with you.",
                            ),
                    ),
            )
    }
}
