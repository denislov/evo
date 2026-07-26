use base64::Engine;

use super::capability::{ImageProtocol, TerminalCapabilities};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

impl Default for CellDimensions {
    fn default() -> Self {
        Self {
            width_px: 9,
            height_px: 18,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCellSize {
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedImage {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: bool,
    pub image_id: Option<u32>,
    pub move_cursor: bool,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub name: Option<String>,
}

impl Default for ImageRenderOptions {
    fn default() -> Self {
        Self {
            max_width_cells: None,
            max_height_cells: None,
            preserve_aspect_ratio: true,
            image_id: None,
            move_cursor: true,
            columns: None,
            rows: None,
            name: None,
        }
    }
}

pub fn is_image_line(line: &str) -> bool {
    line.contains("\x1b_G") || line.contains("\x1b]1337;File=")
}

pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

pub fn encode_kitty(base64_data: &str, options: ImageRenderOptions) -> String {
    const CHUNK_SIZE: usize = 4096;
    let mut params = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];
    if !options.move_cursor {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id {
        params.push(format!("i={image_id}"));
    }

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\x1b_G{};{}\x1b\\", params.join(","), base64_data);
    }

    let mut out = String::new();
    let mut offset = 0usize;
    let mut first = true;
    while offset < base64_data.len() {
        let end = (offset + CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let last = end == base64_data.len();
        if first {
            out.push_str(&format!("\x1b_G{},m=1;{}\x1b\\", params.join(","), chunk));
            first = false;
        } else if last {
            out.push_str(&format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
        offset = end;
    }
    out
}

pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

pub fn encode_iterm2(base64_data: &str, options: ImageRenderOptions) -> String {
    let mut params = vec!["inline=1".to_string()];
    if let Some(columns) = options.columns {
        params.push(format!("width={columns}"));
    }
    if let Some(rows) = options.rows {
        params.push(format!("height={rows}"));
    }
    if !options.preserve_aspect_ratio {
        params.push("preserveAspectRatio=0".to_string());
    }
    format!("\x1b]1337;File={}:{}\x07", params.join(";"), base64_data)
}

pub fn calculate_image_cell_size(
    image: ImageDimensions,
    max_width_cells: u32,
    max_height_cells: Option<u32>,
    cell: CellDimensions,
) -> ImageCellSize {
    let max_width = max_width_cells.max(1);
    let max_height = max_height_cells.map(|height| height.max(1));
    let image_width = image.width_px.max(1) as f64;
    let image_height = image.height_px.max(1) as f64;
    let cell_width = cell.width_px.max(1) as f64;
    let cell_height = cell.height_px.max(1) as f64;
    let width_scale = (max_width as f64 * cell_width) / image_width;
    let height_scale = max_height
        .map(|height| (height as f64 * cell_height) / image_height)
        .unwrap_or(width_scale);
    let scale = width_scale.min(height_scale);
    let columns = ((image_width * scale) / cell_width).ceil() as u32;
    let rows = ((image_height * scale) / cell_height).ceil() as u32;

    ImageCellSize {
        columns: columns.max(1).min(max_width),
        rows: rows.max(1).min(max_height.unwrap_or_else(|| rows.max(1))),
    }
}

pub fn image_dimensions_from_base64(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()?;
    image_dimensions_from_bytes(&bytes, mime_type)
}

pub fn image_dimensions_from_bytes(bytes: &[u8], mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => png_dimensions(bytes),
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/gif" => gif_dimensions(bytes),
        "image/webp" => webp_dimensions(bytes),
        _ => None,
    }
}

pub fn render_image(
    base64_data: &str,
    dimensions: ImageDimensions,
    capabilities: TerminalCapabilities,
    options: ImageRenderOptions,
    cell: CellDimensions,
) -> Option<RenderedImage> {
    let protocol = capabilities.images?;
    let max_width = options.max_width_cells.unwrap_or(80);
    let size = calculate_image_cell_size(dimensions, max_width, options.max_height_cells, cell);
    let mut protocol_options = options.clone();
    protocol_options.columns = Some(size.columns);
    protocol_options.rows = Some(size.rows);
    let sequence = match protocol {
        ImageProtocol::Kitty => encode_kitty(base64_data, protocol_options),
        ImageProtocol::ITerm2 => encode_iterm2(base64_data, protocol_options),
    };
    Some(RenderedImage {
        sequence,
        rows: size.rows,
        image_id: options.image_id,
    })
}

fn png_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    Some(ImageDimensions {
        width_px: u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        height_px: u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    })
}

fn gif_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 10 || (&bytes[0..6] != b"GIF87a" && &bytes[0..6] != b"GIF89a") {
        return None;
    }
    Some(ImageDimensions {
        width_px: u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
        height_px: u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
    })
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 2 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut offset = 2usize;
    while offset + 9 < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            return Some(ImageDimensions {
                height_px: u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().ok()?)
                    as u32,
                width_px: u16::from_be_bytes(bytes[offset + 7..offset + 9].try_into().ok()?) as u32,
            });
        }
        if offset + 3 >= bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes(bytes[offset + 2..offset + 4].try_into().ok()?) as usize;
        if len < 2 {
            return None;
        }
        offset += 2 + len;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 30 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8 " => Some(ImageDimensions {
            width_px: (u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff) as u32,
            height_px: (u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff) as u32,
        }),
        b"VP8L" if bytes.len() >= 25 => {
            let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            Some(ImageDimensions {
                width_px: (bits & 0x3fff) + 1,
                height_px: ((bits >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8X" => Some(ImageDimensions {
            width_px: (u32::from(bytes[24])
                | (u32::from(bytes[25]) << 8)
                | (u32::from(bytes[26]) << 16))
                + 1,
            height_px: (u32::from(bytes[27])
                | (u32::from(bytes[28]) << 8)
                | (u32::from(bytes[29]) << 16))
                + 1,
        }),
        _ => None,
    }
}
