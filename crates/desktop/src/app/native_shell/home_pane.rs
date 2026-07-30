use desktop::shell::SemanticTheme;
use gpui::{IntoElement, ParentElement as _, Render, Role, Styled as _, div, prelude::*, px, rgb};

use super::{
    desktop_style::{DesignSpace, DesktopStyledExt as _},
    evo_brand::{EvoBrand, EvoBrandMode},
};

const HOME_HERO_SHORT_HEIGHT: u32 = 640;
const HOME_HERO_WIDE_WIDTH: u32 = 1_200;
const HOME_HERO_MEDIUM_WIDTH: u32 = 800;

#[derive(Debug, Clone, Copy, PartialEq)]
struct HomeHeroLayout {
    wordmark_width: f32,
    minimum_height: f32,
    vertical_padding: DesignSpace,
    headline_gap: DesignSpace,
    description_gap: DesignSpace,
}

impl HomeHeroLayout {
    fn resolve(viewport_width: u32, viewport_height: u32) -> Self {
        let wordmark_width: f32 = if viewport_width >= HOME_HERO_WIDE_WIDTH {
            360.
        } else if viewport_width >= HOME_HERO_MEDIUM_WIDTH {
            320.
        } else {
            280.
        };
        if viewport_height < HOME_HERO_SHORT_HEIGHT {
            return Self {
                wordmark_width: wordmark_width.min(280.),
                minimum_height: 224.,
                vertical_padding: DesignSpace::Sm,
                headline_gap: DesignSpace::Md,
                description_gap: DesignSpace::Sm,
            };
        }
        Self {
            wordmark_width,
            minimum_height: 320.,
            vertical_padding: DesignSpace::Xl,
            headline_gap: DesignSpace::Xl,
            description_gap: DesignSpace::Md,
        }
    }
}

pub(crate) struct HomePane;

impl HomePane {
    pub(super) fn new() -> Self {
        Self
    }
}

impl Render for HomePane {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let theme = SemanticTheme::GEEK_DARK;
        let viewport = window.viewport_size();
        let layout = HomeHeroLayout::resolve(u32::from(viewport.width), u32::from(viewport.height));

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
            .flex()
            .flex_col()
            .child(
                div()
                    .id("home-hero")
                    .debug_selector(|| "desktop-home-hero".into())
                    .role(Role::Region)
                    .aria_label("Start a new Evo task")
                    .w_full()
                    .max_w(px(900.))
                    .min_h(px(layout.minimum_height))
                    .flex_1()
                    .mx_auto()
                    .px_token(DesignSpace::Xl)
                    .py_token(layout.vertical_padding)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .child(
                        EvoBrand::wordmark(
                            "home-evo-loop",
                            layout.wordmark_width,
                            EvoBrandMode::Dark,
                        )
                        .build()
                        .debug_selector(|| "desktop-evo-wordmark".into()),
                    )
                    .child(
                        div()
                            .id("home-headline")
                            .debug_selector(|| "desktop-home-headline".into())
                            .role(Role::Heading)
                            .mt_token(layout.headline_gap)
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Software evolves. Your agent should too."),
                    )
                    .child(
                        div()
                            .debug_selector(|| "desktop-home-description".into())
                            .mt_token(layout.description_gap)
                            .max_w(px(680.))
                            .text_color(rgb(theme.muted_text.value()))
                            .child(
                                "Describe what you want to build, fix, or understand. Evo will plan, act, and adapt with you.",
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hero_layout_uses_all_contract_wordmark_sizes() {
        assert_eq!(HomeHeroLayout::resolve(1_300, 900).wordmark_width, 360.);
        assert_eq!(HomeHeroLayout::resolve(900, 800).wordmark_width, 320.);
        assert_eq!(HomeHeroLayout::resolve(700, 800).wordmark_width, 280.);
    }

    #[test]
    fn short_height_compacts_the_hero_without_growing_the_wordmark() {
        let regular = HomeHeroLayout::resolve(1_300, 900);
        let short = HomeHeroLayout::resolve(1_300, 480);
        assert_eq!(short.wordmark_width, 280.);
        assert!(short.minimum_height < regular.minimum_height);
        assert_eq!(short.vertical_padding, DesignSpace::Sm);
        assert_eq!(short.headline_gap, DesignSpace::Md);
        assert_eq!(short.description_gap, DesignSpace::Sm);
    }
}
