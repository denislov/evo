use crate::kernel::limits::{
    MAX_IMAGE_DECODE_ALLOC_BYTES, MAX_IMAGE_DECODE_DIMENSION, MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES,
};
use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::io::output::{
    DEFAULT_MAX_BYTES, default_truncation_limit, format_size, truncate_head,
};
use crate::tools::FilesystemTarget;
use crate::tools::args::bounded_arg;
use crate::tools::filesystem_target_for_execution;
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use base64::Engine;
use futures::future::{BoxFuture, FutureExt};
use image::{ImageFormat, ImageReader, Limits};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

const DESCRIPTION: &str = "Read a text or supported image file. Text output is truncated to 2000 lines or 50KB (whichever is hit first); use offset/limit for large text files. JPEG, PNG, GIF, and WebP files return base64 image content.";
const MAX_READ_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_LINE_ARGUMENT: usize = MAX_READ_FILE_BYTES as usize + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageKind {
    mime_type: &'static str,
    format: ImageFormat,
}

fn image_kind(path: &Path) -> Option<ImageKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some(ImageKind {
            mime_type: "image/jpeg",
            format: ImageFormat::Jpeg,
        }),
        "png" => Some(ImageKind {
            mime_type: "image/png",
            format: ImageFormat::Png,
        }),
        "gif" => Some(ImageKind {
            mime_type: "image/gif",
            format: ImageFormat::Gif,
        }),
        "webp" => Some(ImageKind {
            mime_type: "image/webp",
            format: ImageFormat::WebP,
        }),
        _ => None,
    }
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "properties":{
            "path":{"type":"string","description":"Path to the file to read (relative or absolute)"},
            "offset":{"type":"integer","minimum":1,"maximum":MAX_LINE_ARGUMENT,"description":"Line number to start reading from (1-indexed)"},
            "limit":{"type":"integer","minimum":1,"maximum":MAX_LINE_ARGUMENT,"description":"Maximum number of lines to read"}
        },
        "required":["path"]
    })
}

fn text_block(t: String) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: t,
        text_signature: None,
    }]
}

pub trait ReadOperations: Send + Sync {
    fn read_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>>;
}

#[derive(Debug, Default)]
pub struct RealReadOperations;

impl ReadOperations for RealReadOperations {
    fn read_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        let target = target.clone();
        async move {
            tokio::task::spawn_blocking(move || {
            let file = target.opened_file()?;
            let mut file = file
                .lock_resource("read opened file")
                .map_err(|error| error.to_string())?;
            file.seek(SeekFrom::Start(0)).map_err(|error| {
                format!(
                    "read: cannot seek opened file {}: {error}",
                    target.display_path().display()
                )
            })?;
            let metadata = file.metadata().map_err(|error| {
                format!(
                    "read: cannot stat opened file {}: {error}",
                    target.display_path().display()
                )
            })?;
            if metadata.len() > MAX_READ_FILE_BYTES {
                return Err(format!(
                    "read: refusing to read {} because it is {} and exceeds the {} safety limit; use a shell pager or a narrower tool instead",
                    target.display_path().display(),
                    format_size(metadata.len() as usize),
                    format_size(MAX_READ_FILE_BYTES as usize),
                ));
            }
            let mut raw = Vec::with_capacity(
                usize::try_from(metadata.len())
                    .unwrap_or(MAX_READ_FILE_BYTES as usize)
                    .min(MAX_READ_FILE_BYTES as usize),
            );
            file.by_ref()
                .take(MAX_READ_FILE_BYTES + 1)
                .read_to_end(&mut raw)
                .map_err(|error| {
                    format!(
                        "read: cannot read opened file {}: {error}",
                        target.display_path().display()
                    )
                })?;
            if raw.len() > MAX_READ_FILE_BYTES as usize {
                return Err(format!(
                    "read: refusing to retain more than {} from {}",
                    format_size(MAX_READ_FILE_BYTES as usize),
                    target.display_path().display()
                ));
            }
            Ok(raw)
            })
            .await
            .map_err(|error| format!("read: blocking filesystem task failed: {error}"))?
        }
        .boxed()
    }
}

async fn read_target_with_operations(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn ReadOperations>,
) -> Result<Vec<ContentBlock>, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("read: missing or non-string 'path' argument")?
        .to_string();
    // Classification must happen before invoking ReadOperations so an image
    // never enters the text decoding/windowing path after it has been read.
    let image_kind = image_kind(target.display_path());
    let (offset, limit) = if image_kind.is_some() {
        (1, None)
    } else {
        let offset = bounded_arg(&args, "offset", 1, MAX_LINE_ARGUMENT)
            .map(|offset| offset.max(1))
            .map_err(|error| format!("read: {error}"))?;
        let limit = args
            .get("limit")
            .map(|_| {
                bounded_arg(&args, "limit", MAX_LINE_ARGUMENT, MAX_LINE_ARGUMENT)
                    .map(|limit| limit.max(1))
                    .map_err(|error| format!("read: {error}"))
            })
            .transpose()?;
        (offset, limit)
    };
    let raw = ops.read_file(target).await?;
    if let Some(kind) = image_kind {
        return image_blocks(target.display_path(), &raw, kind);
    }

    let content = String::from_utf8_lossy(&raw).into_owned();
    let all: Vec<&str> = content.split('\n').collect();
    let total = all.len();

    let (selected_lines, user_limited) = select_lines(&all, Some(offset), limit)?;
    let selected = selected_lines.join("\n");
    let start = offset.saturating_sub(1);
    let start_display = start.saturating_add(1);

    let tr = truncate_head(&selected, default_truncation_limit());
    let out = if tr.first_line_exceeds_limit {
        let first_line_bytes = all[start].len();
        format!(
            "[Line {start_display} is {}, exceeds {} limit. Use bash: sed -n '{start_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(first_line_bytes),
            format_size(DEFAULT_MAX_BYTES)
        )
    } else if tr.truncated {
        let end_display = start_display
            .saturating_add(tr.output_lines)
            .saturating_sub(1);
        let next = end_display.saturating_add(1);
        if tr.truncated_by.as_deref() == Some("lines") {
            format!(
                "{}\n\n[Showing lines {start_display}-{end_display} of {total}. Use offset={next} to continue.]",
                tr.content
            )
        } else {
            format!(
                "{}\n\n[Showing lines {start_display}-{end_display} of {total} ({} limit). Use offset={next} to continue.]",
                tr.content,
                format_size(DEFAULT_MAX_BYTES)
            )
        }
    } else if let Some(ul) = user_limited {
        let end = start.saturating_add(ul);
        if end < all.len() {
            let remaining = all.len() - end;
            let next = end.saturating_add(1);
            format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next} to continue.]",
                tr.content
            )
        } else {
            tr.content
        }
    } else {
        tr.content
    };

    Ok(text_block(out))
}

fn image_blocks(path: &Path, raw: &[u8], kind: ImageKind) -> Result<Vec<ContentBlock>, String> {
    let encoded_len = raw
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_div(3))
        .and_then(|length| length.checked_mul(4))
        .ok_or_else(|| "read: image size arithmetic overflow".to_owned())?;
    if encoded_len > MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES {
        return Err(format!(
            "read: encoded image exceeds the {} byte safety limit",
            MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES
        ));
    }

    let mut reader = ImageReader::with_format(Cursor::new(raw), kind.format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| {
        format!(
            "read: cannot decode image {} within safety limits: {error}",
            path.display()
        )
    })?;
    let (width, height) = (image.width(), image.height());
    let data = base64::engine::general_purpose::STANDARD.encode(raw);
    Ok(vec![
        ContentBlock::Text {
            text: format!(
                "Read image {} ({}, {width}x{height})",
                path.display(),
                kind.mime_type
            ),
            text_signature: None,
        },
        ContentBlock::Image {
            data,
            mime_type: kind.mime_type.into(),
        },
    ])
}

/// Select the requested line window without arithmetic overflow. A huge
/// user-supplied `limit` (e.g. `u64::MAX` from JSON) previously overflowed
/// `start + l`, wrapping below `start` and panicking on the slice.
fn select_lines<'a>(
    all: &[&'a str],
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<(Vec<&'a str>, Option<usize>), String> {
    let start = offset.unwrap_or(1).saturating_sub(1);
    if start >= all.len() {
        return Err(format!(
            "Offset {} is beyond end of file ({} lines total)",
            offset.unwrap_or(1),
            all.len()
        ));
    }
    let (selected, user_limited) = match limit {
        Some(l) => {
            let end = l.saturating_add(start).min(all.len());
            (&all[start..end], Some(end - start))
        }
        None => (&all[start..], None),
    };
    Ok((selected.to_vec(), user_limited))
}

pub fn read_tool(filesystem: FilesystemCapability) -> AgentTool {
    read_tool_with_operations(filesystem, Arc::new(RealReadOperations))
}

pub fn read_tool_with_operations(
    filesystem: FilesystemCapability,
    ops: Arc<dyn ReadOperations>,
) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        let ops = ops.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let target =
                filesystem_target_for_execution(&filesystem, &context, "read", path).await?;
            read_target_with_operations(&target, args, ops)
                .await
                .map(AgentToolOutput::new)
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "read".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PIXEL_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    fn lines() -> Vec<&'static str> {
        vec!["line1", "line2", "line3"]
    }

    #[test]
    fn huge_limit_with_offset_does_not_panic() {
        let (selected, user_limited) =
            select_lines(&lines(), Some(2), Some(usize::MAX)).expect("selection succeeds");
        assert_eq!(selected, vec!["line2", "line3"]);
        assert_eq!(user_limited, Some(2));
    }

    #[test]
    fn normal_offset_and_limit_are_honored() {
        let (selected, user_limited) =
            select_lines(&lines(), Some(1), Some(1)).expect("selection succeeds");
        assert_eq!(selected, vec!["line1"]);
        assert_eq!(user_limited, Some(1));
    }

    #[test]
    fn missing_offset_starts_at_line_one() {
        let (selected, _) = select_lines(&lines(), None, None).expect("selection succeeds");
        assert_eq!(selected, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn offset_beyond_end_is_rejected() {
        assert!(select_lines(&lines(), Some(99), None).is_err());
        assert!(select_lines(&lines(), Some(0), None).is_ok());
    }

    #[test]
    fn png_read_returns_validated_base64_image_content() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(ONE_PIXEL_PNG_BASE64)
            .expect("decode png fixture");
        let blocks = image_blocks(
            Path::new("pixel.PNG"),
            &raw,
            image_kind(Path::new("pixel.PNG")).expect("supported extension"),
        )
        .expect("read valid image");
        assert!(matches!(
            blocks.as_slice(),
            [ContentBlock::Text { text, .. }, ContentBlock::Image { data, mime_type }]
                if text.contains("1x1")
                    && data == ONE_PIXEL_PNG_BASE64
                    && mime_type == "image/png"
        ));
    }

    #[test]
    fn invalid_image_payload_is_rejected_instead_of_forwarded() {
        let error = image_blocks(
            Path::new("not-an-image.webp"),
            b"not an image",
            image_kind(Path::new("not-an-image.webp")).expect("supported extension"),
        )
        .expect_err("invalid image must fail closed");
        assert!(error.contains("cannot decode image"));
    }
}
