use crate::component::Component;
use crate::render::truncate_to_width;
use crate::render::{color_enabled, paint_with};
use crate::terminal::{
    CellDimensions, ImageDimensions, ImageRenderOptions, TerminalCapabilities,
    image_dimensions_from_base64, render_image,
};
use crate::theme::ImageTheme;

pub struct Image {
    base64_data: String,
    mime_type: String,
    filename: Option<String>,
    dimensions: Option<ImageDimensions>,
    capabilities: TerminalCapabilities,
    cell_dimensions: CellDimensions,
    max_width_cells: Option<u32>,
    max_height_cells: Option<u32>,
    image_id: Option<u32>,
    image_theme: ImageTheme,
}

impl Image {
    pub fn new(base64_data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            base64_data: base64_data.into(),
            mime_type: mime_type.into(),
            filename: None,
            dimensions: None,
            capabilities: TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false,
            },
            cell_dimensions: CellDimensions::default(),
            max_width_cells: None,
            max_height_cells: None,
            image_id: None,
            image_theme: ImageTheme::default(),
        }
    }

    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    pub fn dimensions(mut self, dimensions: ImageDimensions) -> Self {
        self.dimensions = Some(dimensions);
        self
    }

    pub fn capabilities(mut self, capabilities: TerminalCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn cell_dimensions(mut self, dimensions: CellDimensions) -> Self {
        self.cell_dimensions = dimensions;
        self
    }

    pub fn max_width_cells(mut self, max_width_cells: u32) -> Self {
        self.max_width_cells = Some(max_width_cells);
        self
    }

    pub fn max_height_cells(mut self, max_height_cells: u32) -> Self {
        self.max_height_cells = Some(max_height_cells);
        self
    }

    pub fn image_theme(mut self, theme: ImageTheme) -> Self {
        self.image_theme = theme;
        self
    }

    pub fn image_id(mut self, image_id: u32) -> Self {
        self.image_id = Some(image_id);
        self
    }

    fn dimensions_or_parse(&self) -> Option<ImageDimensions> {
        self.dimensions
            .or_else(|| image_dimensions_from_base64(&self.base64_data, &self.mime_type))
    }

    fn fallback(&self, width: usize) -> String {
        let mut parts = Vec::new();
        if let Some(filename) = &self.filename {
            parts.push(filename.clone());
        }
        parts.push(format!("[{}]", self.mime_type));
        if let Some(dimensions) = self.dimensions_or_parse() {
            parts.push(format!("{}x{}", dimensions.width_px, dimensions.height_px));
        }
        let text = truncate_to_width(&format!("[Image: {}]", parts.join(" ")), width);
        paint_with(&text, &self.image_theme.fallback_color, color_enabled())
    }
}

impl Component for Image {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }

        let Some(dimensions) = self.dimensions_or_parse() else {
            return vec![self.fallback(width)];
        };
        match render_image(
            &self.base64_data,
            dimensions,
            self.capabilities,
            ImageRenderOptions {
                max_width_cells: self.max_width_cells.or(Some(width as u32)),
                max_height_cells: self.max_height_cells,
                image_id: self.image_id,
                ..Default::default()
            },
            self.cell_dimensions,
        ) {
            Some(rendered) => {
                let mut lines = vec![rendered.sequence];
                lines.resize(rendered.rows.max(1) as usize, String::new());
                lines
            }
            None => vec![self.fallback(width)],
        }
    }
}
