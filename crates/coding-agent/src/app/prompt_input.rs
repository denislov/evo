use crate::app::error::ApplicationError;
use crate::limits::{
    MAX_AT_FILE_BYTES, MAX_AT_FILE_REFERENCES, MAX_IMAGE_DECODE_ALLOC_BYTES,
    MAX_IMAGE_DECODE_DIMENSION, MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES, MAX_INPUT_IMAGES,
    MAX_PROMPT_INPUT_BYTES,
};
use crate::platform::io::bounded as bounded_io;
use ai::api::conversation::ContentBlock;
use base64::Engine;
use image::{GenericImageView, ImageFormat, ImageReader, Limits};
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedPromptInput {
    pub text: String,
    pub images: Vec<ImageAttachment>,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageResizeOptions {
    pub enabled: bool,
    pub max_dimension: u32,
}

impl Default for ImageResizeOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            max_dimension: 2000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageProcessingOptions {
    pub resize: ImageResizeOptions,
    pub block_images: bool,
}

impl ImageProcessingOptions {
    pub const fn new(auto_resize_images: bool, block_images: bool) -> Self {
        Self {
            resize: ImageResizeOptions {
                enabled: auto_resize_images,
                max_dimension: 2000,
            },
            block_images,
        }
    }

    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        Self::new(
            settings.terminal.auto_resize_images,
            settings.terminal.block_images,
        )
    }
}

pub fn merge_stdin_prompt(prompt: &str, stdin: Option<&str>) -> String {
    let prompt = prompt.trim_end();
    let stdin = stdin.map(str::trim_end).filter(|value| !value.is_empty());
    match (prompt.is_empty(), stdin) {
        (_, None) => prompt.to_string(),
        (true, Some(stdin)) => stdin.to_string(),
        (false, Some(stdin)) => format!("{prompt}\n\n{stdin}"),
    }
}

pub fn process_at_file_references_with_processing_options(
    prompt: &str,
    cwd: &Path,
    options: ImageProcessingOptions,
) -> Result<ProcessedPromptInput, ApplicationError> {
    if prompt.len() > MAX_PROMPT_INPUT_BYTES {
        return Err(ApplicationError::InvalidInput(format!(
            "prompt exceeds the {MAX_PROMPT_INPUT_BYTES} byte safety limit"
        )));
    }
    let mut text = String::new();
    let mut images = Vec::new();
    let mut chars = prompt.char_indices().peekable();
    let mut file_references = 0usize;

    while let Some((_, ch)) = chars.next() {
        if ch != '@' {
            text.push(ch);
            continue;
        }

        let Some((_, next)) = chars.peek().copied() else {
            text.push('@');
            continue;
        };

        let path = if next == '"' || next == '\'' {
            chars.next();
            let quote = next;
            let mut path = String::new();
            let mut closed = false;
            for (_, quoted_ch) in chars.by_ref() {
                if quoted_ch == quote {
                    closed = true;
                    break;
                }
                path.push(quoted_ch);
            }
            if !closed {
                text.push('@');
                text.push(quote);
                text.push_str(&path);
                continue;
            }
            path
        } else {
            let mut path = String::new();
            while let Some((_, path_ch)) = chars.peek().copied() {
                if path_ch.is_whitespace() {
                    break;
                }
                chars.next();
                path.push(path_ch);
            }
            path
        };

        if path.is_empty() {
            text.push('@');
            continue;
        }

        file_references = file_references.saturating_add(1);
        if file_references > MAX_AT_FILE_REFERENCES {
            return Err(ApplicationError::InvalidInput(format!(
                "prompt contains more than {MAX_AT_FILE_REFERENCES} @file references"
            )));
        }
        append_file_reference(&mut text, &mut images, &path, cwd, options)?;
        if text.len() > MAX_PROMPT_INPUT_BYTES {
            return Err(ApplicationError::InvalidInput(format!(
                "expanded prompt exceeds the {MAX_PROMPT_INPUT_BYTES} byte safety limit"
            )));
        }
    }

    let mut content = vec![ContentBlock::Text {
        text: text.clone(),
        text_signature: None,
    }];
    content.extend(images.iter().map(|image| ContentBlock::Image {
        data: image.data.clone(),
        mime_type: image.mime_type.clone(),
    }));

    Ok(ProcessedPromptInput {
        text,
        images,
        content,
    })
}

pub(crate) fn process_explicit_file_attachments(
    prompt: &str,
    attachments: &[PathBuf],
    cwd: &Path,
    options: ImageProcessingOptions,
) -> Result<ProcessedPromptInput, ApplicationError> {
    if prompt.len() > MAX_PROMPT_INPUT_BYTES {
        return Err(ApplicationError::InvalidInput(format!(
            "prompt exceeds the {MAX_PROMPT_INPUT_BYTES} byte safety limit"
        )));
    }
    if attachments.len() > MAX_AT_FILE_REFERENCES {
        return Err(ApplicationError::InvalidInput(format!(
            "prompt contains more than {MAX_AT_FILE_REFERENCES} file attachments"
        )));
    }

    let mut text = prompt.to_owned();
    let mut images = Vec::new();
    for attachment in attachments {
        let path = attachment.to_str().ok_or_else(|| {
            ApplicationError::InvalidInput("attachment path must be valid UTF-8".into())
        })?;
        if !text.is_empty() {
            text.push('\n');
        }
        append_file_reference(&mut text, &mut images, path, cwd, options)?;
        if text.len() > MAX_PROMPT_INPUT_BYTES {
            return Err(ApplicationError::InvalidInput(format!(
                "expanded prompt exceeds the {MAX_PROMPT_INPUT_BYTES} byte safety limit"
            )));
        }
    }

    let mut content = vec![ContentBlock::Text {
        text: text.clone(),
        text_signature: None,
    }];
    content.extend(images.iter().map(|image| ContentBlock::Image {
        data: image.data.clone(),
        mime_type: image.mime_type.clone(),
    }));
    Ok(ProcessedPromptInput {
        text,
        images,
        content,
    })
}

fn append_file_reference(
    text: &mut String,
    images: &mut Vec<ImageAttachment>,
    path: &str,
    cwd: &Path,
    options: ImageProcessingOptions,
) -> Result<(), ApplicationError> {
    let path = resolve_input_path(path, cwd);
    let bytes = bounded_io::read_file(&path, MAX_AT_FILE_BYTES).map_err(|error| {
        ApplicationError::InvalidInput(format!(
            "failed to read @file {}: {}",
            path.display(),
            error
        ))
    })?;
    if bytes.is_empty() {
        return Ok(());
    }

    if let Some(mime_type) = detect_image_mime(&path, &bytes) {
        if options.block_images {
            text.push_str(&format!("<file name=\"{}\"></file>", path.display()));
            text.push_str("\n[Image reading is disabled.]");
            return Ok(());
        }

        if images.len() >= MAX_INPUT_IMAGES {
            return Err(ApplicationError::InvalidInput(format!(
                "prompt contains more than {MAX_INPUT_IMAGES} images"
            )));
        }
        let resized = resize_image_bytes(&bytes, mime_type, options.resize)?;
        let encoded_len = resized
            .bytes
            .len()
            .checked_add(2)
            .and_then(|length| length.checked_div(3))
            .and_then(|length| length.checked_mul(4))
            .ok_or_else(|| {
                ApplicationError::InvalidInput("image size arithmetic overflow".into())
            })?;
        let retained_image_bytes = images
            .iter()
            .try_fold(0usize, |total, image| total.checked_add(image.data.len()))
            .and_then(|total| total.checked_add(encoded_len))
            .ok_or_else(|| {
                ApplicationError::InvalidInput("aggregate image size overflow".into())
            })?;
        if retained_image_bytes > MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES {
            return Err(ApplicationError::InvalidInput(format!(
                "encoded images exceed the {MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES} byte aggregate safety limit"
            )));
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&resized.bytes);
        images.push(ImageAttachment {
            data,
            mime_type: mime_type.to_string(),
        });
        text.push_str(&format!("<file name=\"{}\"></file>", path.display()));
        if let Some(note) = resized.note {
            text.push_str(&format!("\n[{}]", note));
        }
    } else {
        let content = String::from_utf8(bytes).map_err(|error| {
            ApplicationError::InvalidInput(format!(
                "failed to decode @file {} as UTF-8: {}",
                path.display(),
                error
            ))
        })?;
        text.push_str(&format!(
            "<file name=\"{}\">\n{}\n</file>",
            path.display(),
            content
        ));
    }
    Ok(())
}

struct ResizedImageBytes {
    bytes: Vec<u8>,
    note: Option<String>,
}

fn resize_image_bytes(
    bytes: &[u8],
    mime_type: &str,
    options: ImageResizeOptions,
) -> Result<ResizedImageBytes, ApplicationError> {
    if !options.enabled || options.max_dimension == 0 {
        return Ok(ResizedImageBytes {
            bytes: bytes.to_vec(),
            note: None,
        });
    }

    let Some(format) = image_format_for_mime(mime_type) else {
        return Ok(ResizedImageBytes {
            bytes: bytes.to_vec(),
            note: None,
        });
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(_) => {
            return Ok(ResizedImageBytes {
                bytes: bytes.to_vec(),
                note: None,
            });
        }
    };
    let (width, height) = image.dimensions();
    let max_dimension = options.max_dimension;
    if width <= max_dimension && height <= max_dimension {
        return Ok(ResizedImageBytes {
            bytes: bytes.to_vec(),
            note: None,
        });
    }

    let scale = (max_dimension as f64 / width as f64).min(max_dimension as f64 / height as f64);
    let new_width = ((width as f64 * scale).round() as u32).max(1);
    let new_height = ((height as f64 * scale).round() as u32).max(1);
    let resized = image.resize_exact(new_width, new_height, image::imageops::FilterType::Triangle);
    let mut output = Cursor::new(Vec::new());
    resized.write_to(&mut output, format).map_err(|error| {
        ApplicationError::InvalidInput(format!("failed to encode resized image: {error}"))
    })?;

    Ok(ResizedImageBytes {
        bytes: output.into_inner(),
        note: Some(format!(
            "image resized from {}x{} to {}x{}",
            width, height, new_width, new_height
        )),
    })
}

fn image_format_for_mime(mime_type: &str) -> Option<ImageFormat> {
    match mime_type {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

fn resolve_input_path(path: &str, cwd: &Path) -> PathBuf {
    let expanded = if path == "~" {
        dirs::home_dir().unwrap_or_else(|| cwd.to_path_buf())
    } else if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| cwd.to_path_buf())
            .join(rest)
    } else {
        PathBuf::from(path)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

fn detect_image_mime(path: &Path, bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}
