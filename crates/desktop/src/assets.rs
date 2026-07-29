use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub(crate) const EVO_WORDMARK_PATH: &str = "brand/evo-loop-wordmark.svg";
pub(crate) const EVO_WORDMARK_ACCENT_PATH: &str = "brand/evo-loop-wordmark-accent.svg";
pub(crate) const EVO_COMPACT_PATH: &str = "brand/evo-loop-compact.svg";
pub(crate) const EVO_COMPACT_ACCENT_PATH: &str = "brand/evo-loop-compact-accent.svg";

const EVO_WORDMARK: &[u8] = include_bytes!("../assets/brand/evo-loop-wordmark.svg");
const EVO_WORDMARK_ACCENT: &[u8] = include_bytes!("../assets/brand/evo-loop-wordmark-accent.svg");
const EVO_COMPACT: &[u8] = include_bytes!("../assets/brand/evo-loop-compact.svg");
const EVO_COMPACT_ACCENT: &[u8] = include_bytes!("../assets/brand/evo-loop-compact-accent.svg");

const EVO_ASSET_PATHS: [&str; 4] = [
    EVO_WORDMARK_PATH,
    EVO_WORDMARK_ACCENT_PATH,
    EVO_COMPACT_PATH,
    EVO_COMPACT_ACCENT_PATH,
];

/// The application asset boundary: product-owned Evo vectors plus the pinned
/// component icon bundle. Panes never load files or know where either source
/// is stored.
pub(crate) struct DesktopAssets {
    component_assets: gpui_component_assets::Assets,
}

impl DesktopAssets {
    pub(crate) fn new() -> Self {
        Self {
            component_assets: gpui_component_assets::Assets::new(""),
        }
    }
}

impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let brand = match path {
            EVO_WORDMARK_PATH => Some(EVO_WORDMARK),
            EVO_WORDMARK_ACCENT_PATH => Some(EVO_WORDMARK_ACCENT),
            EVO_COMPACT_PATH => Some(EVO_COMPACT),
            EVO_COMPACT_ACCENT_PATH => Some(EVO_COMPACT_ACCENT),
            _ => None,
        };
        if let Some(bytes) = brand {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        self.component_assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = self.component_assets.list(path)?;
        assets.extend(
            EVO_ASSET_PATHS
                .into_iter()
                .filter(|asset| asset.starts_with(path))
                .map(SharedString::from),
        );
        assets.sort_unstable();
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evo_assets_are_path_only_svg_masks_without_font_or_raster_fallbacks() {
        let assets = DesktopAssets::new();
        for path in EVO_ASSET_PATHS {
            let bytes = assets
                .load(path)
                .unwrap()
                .unwrap_or_else(|| panic!("missing embedded Evo asset {path}"));
            let source = std::str::from_utf8(&bytes).unwrap();
            assert!(source.contains("<svg"));
            assert!(source.contains("<path"));
            for forbidden in [
                "<text",
                "font-",
                "font-family",
                "<image",
                ".png",
                ".jpg",
                "<script",
                "<animate",
                "<filter",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{path} must not contain {forbidden}"
                );
            }
        }
    }

    #[test]
    fn desktop_assets_preserve_the_component_icon_source() {
        let assets = DesktopAssets::new();
        assert!(assets.load("icons/plus.svg").unwrap().is_some());
        let brand = assets.list("brand/").unwrap();
        assert_eq!(brand.len(), EVO_ASSET_PATHS.len());
        for path in EVO_ASSET_PATHS {
            assert!(brand.iter().any(|asset| asset.as_ref() == path));
        }
    }
}
