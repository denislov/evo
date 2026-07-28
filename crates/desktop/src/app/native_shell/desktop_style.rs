use desktop::shell::DESKTOP_DESIGN_TOKENS;
use gpui::{Styled, px};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesignSpace {
    Xs,
    Sm,
    Md,
    Lg,
    Xl,
}

impl DesignSpace {
    const fn pixels(self) -> f32 {
        let spacing = DESKTOP_DESIGN_TOKENS.spacing;
        match self {
            Self::Xs => spacing.xs as f32,
            Self::Sm => spacing.sm as f32,
            Self::Md => spacing.md as f32,
            Self::Lg => spacing.lg as f32,
            Self::Xl => spacing.xl as f32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesignRadius {
    Sm,
    Md,
    Lg,
}

impl DesignRadius {
    const fn pixels(self) -> f32 {
        let radius = DESKTOP_DESIGN_TOKENS.radius;
        match self {
            Self::Sm => radius.sm as f32,
            Self::Md => radius.md as f32,
            Self::Lg => radius.lg as f32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesignText {
    Metadata,
    Body,
    Title,
}

impl DesignText {
    const fn metrics(self) -> (f32, f32) {
        let typography = DESKTOP_DESIGN_TOKENS.typography;
        match self {
            Self::Metadata => (
                typography.metadata_size as f32,
                typography.metadata_line_height as f32,
            ),
            Self::Body => (
                typography.body_size as f32,
                typography.body_line_height as f32,
            ),
            Self::Title => (
                typography.title_size as f32,
                typography.title_line_height as f32,
            ),
        }
    }
}

pub(super) trait DesktopStyledExt: Styled + Sized {
    fn p_token(self, spacing: DesignSpace) -> Self {
        self.p(px(spacing.pixels()))
    }

    fn px_token(self, spacing: DesignSpace) -> Self {
        self.px(px(spacing.pixels()))
    }

    fn py_token(self, spacing: DesignSpace) -> Self {
        self.py(px(spacing.pixels()))
    }

    fn pl_token(self, spacing: DesignSpace) -> Self {
        self.pl(px(spacing.pixels()))
    }

    fn gap_token(self, spacing: DesignSpace) -> Self {
        self.gap(px(spacing.pixels()))
    }

    fn mt_token(self, spacing: DesignSpace) -> Self {
        self.mt(px(spacing.pixels()))
    }

    fn mb_token(self, spacing: DesignSpace) -> Self {
        self.mb(px(spacing.pixels()))
    }

    fn rounded_token(self, radius: DesignRadius) -> Self {
        self.rounded(px(radius.pixels()))
    }

    fn text_token(self, text: DesignText) -> Self {
        let (size, line_height) = text.metrics();
        self.text_size(px(size)).line_height(px(line_height))
    }
}

impl<T: Styled> DesktopStyledExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_values_are_exactly_backed_by_public_design_tokens() {
        assert_eq!(DesignSpace::Xs.pixels(), 4.);
        assert_eq!(DesignSpace::Sm.pixels(), 8.);
        assert_eq!(DesignSpace::Md.pixels(), 12.);
        assert_eq!(DesignSpace::Lg.pixels(), 16.);
        assert_eq!(DesignSpace::Xl.pixels(), 24.);
        assert_eq!(DesignRadius::Sm.pixels(), 4.);
        assert_eq!(DesignRadius::Md.pixels(), 6.);
        assert_eq!(DesignRadius::Lg.pixels(), 8.);
        assert_eq!(DesignText::Metadata.metrics(), (12., 16.));
        assert_eq!(DesignText::Body.metrics(), (14., 21.));
        assert_eq!(DesignText::Title.metrics(), (16., 24.));
    }
}
