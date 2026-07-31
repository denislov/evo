use desktop::ui::shell::{SemanticColor, SemanticTheme};
use gpui::{
    ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb, svg,
};

use crate::assets::{
    EVO_COMPACT_ACCENT_PATH, EVO_COMPACT_PATH, EVO_WORDMARK_ACCENT_PATH, EVO_WORDMARK_PATH,
};

use super::style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _};

const WORDMARK_VIEWBOX_WIDTH: f32 = 360.;
const WORDMARK_VIEWBOX_HEIGHT: f32 = 128.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvoBrandVariant {
    Wordmark,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvoBrandMode {
    Dark,
    Light,
    Monochrome,
}

impl EvoBrandMode {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Monochrome => "monochrome",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            "monochrome" => Ok(Self::Monochrome),
            other => Err(format!(
                "Evo brand fixture mode must be dark, light, or monochrome; got {other}"
            )),
        }
    }

    pub(crate) const fn tokens(self) -> EvoBrandTokens {
        match self {
            Self::Dark => EvoBrandTokens {
                canvas: SemanticTheme::GEEK_DARK.canvas,
                foreground: SemanticTheme::GEEK_DARK.text,
                accent: SemanticTheme::GEEK_DARK.accent,
                metadata: SemanticTheme::GEEK_DARK.muted_text,
                border: SemanticTheme::GEEK_DARK.border,
            },
            Self::Light => EvoBrandTokens {
                canvas: SemanticColor::rgb(0xf8fafc),
                foreground: SemanticColor::rgb(0x172033),
                accent: SemanticColor::rgb(0x2563eb),
                metadata: SemanticColor::rgb(0x526174),
                border: SemanticColor::rgb(0xd7dee8),
            },
            Self::Monochrome => EvoBrandTokens {
                canvas: SemanticColor::rgb(0xffffff),
                foreground: SemanticColor::rgb(0x111111),
                accent: SemanticColor::rgb(0x111111),
                metadata: SemanticColor::rgb(0x444444),
                border: SemanticColor::rgb(0xb8b8b8),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvoBrandTokens {
    pub(crate) canvas: SemanticColor,
    pub(crate) foreground: SemanticColor,
    pub(crate) accent: SemanticColor,
    pub(crate) metadata: SemanticColor,
    pub(crate) border: SemanticColor,
}

pub(crate) struct EvoBrand {
    id: ElementId,
    variant: EvoBrandVariant,
    width: f32,
    mode: EvoBrandMode,
}

impl EvoBrand {
    pub(crate) fn wordmark(id: impl Into<ElementId>, width: f32, mode: EvoBrandMode) -> Self {
        Self {
            id: id.into(),
            variant: EvoBrandVariant::Wordmark,
            width,
            mode,
        }
    }

    pub(crate) fn compact(id: impl Into<ElementId>, side: f32, mode: EvoBrandMode) -> Self {
        Self {
            id: id.into(),
            variant: EvoBrandVariant::Compact,
            width: side,
            mode,
        }
    }

    pub(crate) const fn dimensions(&self) -> (f32, f32) {
        match self.variant {
            EvoBrandVariant::Wordmark => (
                self.width,
                self.width * WORDMARK_VIEWBOX_HEIGHT / WORDMARK_VIEWBOX_WIDTH,
            ),
            EvoBrandVariant::Compact => (self.width, self.width),
        }
    }

    fn paths(&self) -> (&'static str, &'static str) {
        match self.variant {
            EvoBrandVariant::Wordmark => (EVO_WORDMARK_PATH, EVO_WORDMARK_ACCENT_PATH),
            EvoBrandVariant::Compact => (EVO_COMPACT_PATH, EVO_COMPACT_ACCENT_PATH),
        }
    }

    pub(crate) fn build(self) -> gpui::Stateful<gpui::Div> {
        let (width, height) = self.dimensions();
        let (body_path, accent_path) = self.paths();
        let tokens = self.mode.tokens();
        let accessible_label = match self.variant {
            EvoBrandVariant::Wordmark => "Evo wordmark",
            EvoBrandVariant::Compact => "Evo compact mark",
        };

        div()
            .id(self.id)
            .role(Role::Image)
            .aria_label(accessible_label)
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .child(
                svg()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .path(body_path)
                    .text_color(rgb(tokens.foreground.value())),
            )
            .child(
                svg()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .path(accent_path)
                    .text_color(rgb(tokens.accent.value())),
            )
    }
}

/// Deterministic board used by the VUI-411 capture script. Every mark is
/// rendered by the same production component at an exact contract size.
pub(crate) struct EvoBrandFixture {
    mode: EvoBrandMode,
}

impl EvoBrandFixture {
    pub(crate) const fn new(mode: EvoBrandMode) -> Self {
        Self { mode }
    }
}

impl Render for EvoBrandFixture {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        let tokens = mode.tokens();
        let compact = [(16., 16usize), (24., 24usize), (32., 32usize)]
            .into_iter()
            .map(|(side, key)| {
                div()
                    .w(px(120.))
                    .h(px(96.))
                    .rounded_token(DesignRadius::Md)
                    .border_1()
                    .border_color(rgb(tokens.border.value()))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        EvoBrand::compact(("brand-compact", key), side, mode)
                            .build()
                            .debug_selector(move || format!("desktop-brand-compact-{key}")),
                    )
                    .child(
                        div()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(tokens.metadata.value()))
                            .child(format!("{key} px")),
                    )
            });

        div()
            .id("evo-brand-fixture")
            .debug_selector(|| "desktop-brand-fixture".into())
            .size_full()
            .bg(rgb(tokens.canvas.value()))
            .text_color(rgb(tokens.foreground.value()))
            .p_token(DesignSpace::Xl)
            .flex()
            .flex_col()
            .gap_token(DesignSpace::Xl)
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Xs)
                            .child(div().text_size(px(26.)).child("Evo Loop vector fixtures"))
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(tokens.metadata.value()))
                                    .child("Path-only SVG · static · production renderer"),
                            ),
                    )
                    .child(
                        div()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(tokens.metadata.value()))
                            .child(mode.key()),
                    ),
            )
            .child(
                div()
                    .text_token(DesignText::Metadata)
                    .text_color(rgb(tokens.metadata.value()))
                    .child("COMPACT MARK"),
            )
            .child(div().flex().gap_token(DesignSpace::Lg).children(compact))
            .child(
                div()
                    .text_token(DesignText::Metadata)
                    .text_color(rgb(tokens.metadata.value()))
                    .child("FULL WORDMARK"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Lg)
                    .child(
                        EvoBrand::wordmark("brand-wordmark-200", 200., mode)
                            .build()
                            .debug_selector(|| "desktop-brand-wordmark-200".into()),
                    )
                    .child(
                        EvoBrand::wordmark("brand-wordmark-360", 360., mode)
                            .build()
                            .debug_selector(|| "desktop-brand-wordmark-360".into()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, size};
    use gpui_component::{Theme, ThemeMode};

    #[test]
    fn production_sizes_preserve_their_exact_vector_aspect_ratios() {
        for side in [16., 24., 32.] {
            assert_eq!(
                EvoBrand::compact("compact", side, EvoBrandMode::Dark).dimensions(),
                (side, side)
            );
        }
        assert_eq!(
            EvoBrand::wordmark("wordmark", 200., EvoBrandMode::Dark).dimensions(),
            (200., 200. * 128. / 360.)
        );
        assert_eq!(
            EvoBrand::wordmark("wordmark", 360., EvoBrandMode::Dark).dimensions(),
            (360., 128.)
        );
    }

    #[test]
    fn dark_light_and_monochrome_tokens_are_explicit_and_readable() {
        for mode in [
            EvoBrandMode::Dark,
            EvoBrandMode::Light,
            EvoBrandMode::Monochrome,
        ] {
            let tokens = mode.tokens();
            assert!(tokens.foreground.contrast_ratio(tokens.canvas) >= 4.5);
            assert!(tokens.accent.contrast_ratio(tokens.canvas) >= 4.5);
        }
        let monochrome = EvoBrandMode::Monochrome.tokens();
        assert_eq!(monochrome.foreground, monochrome.accent);
        assert_ne!(
            EvoBrandMode::Dark.tokens().foreground,
            EvoBrandMode::Dark.tokens().accent
        );
        assert_ne!(
            EvoBrandMode::Light.tokens().foreground,
            EvoBrandMode::Light.tokens().accent
        );
    }

    #[test]
    fn fixture_modes_are_typed_and_reject_unknown_values() {
        assert_eq!(EvoBrandMode::parse("dark"), Ok(EvoBrandMode::Dark));
        assert_eq!(EvoBrandMode::parse("light"), Ok(EvoBrandMode::Light));
        assert_eq!(
            EvoBrandMode::parse("monochrome"),
            Ok(EvoBrandMode::Monochrome)
        );
        assert!(EvoBrandMode::parse("sepia").is_err());
    }

    #[gpui::test]
    fn production_fixture_uses_raster_aligned_contract_bounds(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| EvoBrandFixture::new(EvoBrandMode::Dark));
        cx.simulate_resize(size(px(900.), px(700.)));
        cx.run_until_parked();

        for (selector, width, height) in [
            ("desktop-brand-compact-16", 16., 16.),
            ("desktop-brand-compact-24", 24., 24.),
            ("desktop-brand-compact-32", 32., 32.),
            // GPUI aligns the 71.111 px theoretical vector height to the
            // nearest device pixel. The component-level test above retains the
            // exact 360:128 aspect-ratio contract before layout rasterization.
            ("desktop-brand-wordmark-200", 200., 71.),
            ("desktop-brand-wordmark-360", 360., 128.),
        ] {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing brand fixture {selector}"));
            assert_eq!(f32::from(bounds.size.width), width);
            assert_eq!(f32::from(bounds.size.height), height);
        }
    }
}
