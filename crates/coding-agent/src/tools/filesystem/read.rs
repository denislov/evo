use crate::kernel::limits::{
    MAX_IMAGE_DECODE_ALLOC_BYTES, MAX_IMAGE_DECODE_DIMENSION, MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES,
};
use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::io::output::{
    DEFAULT_MAX_BYTES, default_truncation_limit, format_size, truncate_head,
};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::hashline::line_hash;
use crate::tools::filesystem_target_for_runtime_execution;
use base64::Engine;
use futures::future::{BoxFuture, FutureExt};
use image::{ImageFormat, ImageReader, Limits};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};

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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// Path to the file to read (relative or absolute).
    path: String,
    /// Line number to start reading from (1-indexed).
    #[schemars(range(min = 1, max = 5_242_881))]
    #[serde(default, deserialize_with = "deserialize_optional_line_argument")]
    offset: Option<u64>,
    /// Maximum number of lines to read.
    #[schemars(range(min = 1, max = 5_242_881))]
    #[serde(default, deserialize_with = "deserialize_optional_line_argument")]
    limit: Option<u64>,
}

fn deserialize_optional_line_argument<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value == 0 || value > MAX_LINE_ARGUMENT as u64) {
        return Err(serde::de::Error::custom(format!(
            "line argument must be between 1 and {MAX_LINE_ARGUMENT}"
        )));
    }
    Ok(value)
}

impl ReadArgs {
    fn line_window(&self) -> Result<(usize, Option<usize>), ToolError> {
        let offset = bounded_line_argument("offset", self.offset.unwrap_or(1))?;
        let limit = self
            .limit
            .map(|limit| bounded_line_argument("limit", limit))
            .transpose()?;
        Ok((offset, limit))
    }
}

fn bounded_line_argument(name: &str, value: u64) -> Result<usize, ToolError> {
    if value == 0 || value > MAX_LINE_ARGUMENT as u64 {
        return Err(ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("read: {name} must be between 1 and {MAX_LINE_ARGUMENT}"),
        ));
    }
    Ok(value as usize)
}

fn text_block(text: String) -> Vec<ToolContent> {
    vec![ToolContent::Text { text }]
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
    args: ReadArgs,
    ops: Arc<dyn ReadOperations>,
) -> Result<ToolOutput, ToolError> {
    let path = args.path.clone();
    // Classification must happen before invoking ReadOperations so an image
    // never enters the text decoding/windowing path after it has been read.
    let image_kind = image_kind(target.display_path());
    let requested_window = args.line_window()?;
    let (offset, limit) = if image_kind.is_some() {
        (1, None)
    } else {
        requested_window
    };
    let raw = ops
        .read_file(target)
        .await
        .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
    let content = if let Some(kind) = image_kind {
        image_blocks(target.display_path(), &raw, kind)?
    } else {
        text_blocks(&path, &raw, offset, limit)?
    };
    let content_sha256 = format!("{:x}", Sha256::digest(&raw));
    let hashline = if image_kind.is_none() {
        std::str::from_utf8(&raw)
            .ok()
            .map(|text| hashline_window(text, offset, limit))
    } else {
        None
    };
    let mut details = serde_json::json!({
        "path": path,
        "target_fingerprint": target.target_fingerprint(),
        "content_sha256": content_sha256,
        "bytes": raw.len(),
    });
    if let Some(hashline) = hashline {
        details["hashline"] = serde_json::json!(hashline);
    }
    Ok(ToolOutput {
        content,
        details: Some(details),
        terminate: false,
    })
}

fn hashline_window(content: &str, offset: usize, limit: Option<usize>) -> Vec<String> {
    let start = offset.saturating_sub(1);
    let requested = limit.unwrap_or(usize::MAX);
    let mut encoded_bytes = 0usize;
    let mut anchors = Vec::new();
    let content = content.strip_suffix('\n').unwrap_or(content);
    for (index, text) in content
        .split('\n')
        .enumerate()
        .skip(start)
        .take(requested)
        .take(crate::limits::MAX_HASHLINE_DETAIL_LINES)
    {
        let anchor = format!("{}:{}→{}", index + 1, line_hash(text), text);
        let next_bytes = encoded_bytes
            .checked_add(anchor.len())
            .and_then(|bytes| bytes.checked_add(1));
        if next_bytes.is_none_or(|bytes| bytes > crate::limits::MAX_HASHLINE_DETAILS_BYTES) {
            break;
        }
        encoded_bytes = next_bytes.expect("bounded hashline size was checked");
        anchors.push(anchor);
    }
    anchors
}

fn text_blocks(
    path: &str,
    raw: &[u8],
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<ToolContent>, ToolError> {
    let content = String::from_utf8_lossy(raw).into_owned();
    let all: Vec<&str> = content.split('\n').collect();
    let total = all.len();

    let (selected_lines, user_limited) = select_lines(&all, Some(offset), limit)
        .map_err(|error| ToolError::new(ToolErrorKind::InvalidArguments, error))?;
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

fn image_blocks(path: &Path, raw: &[u8], kind: ImageKind) -> Result<Vec<ToolContent>, ToolError> {
    let encoded_len = raw
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_div(3))
        .and_then(|length| length.checked_mul(4))
        .ok_or_else(|| {
            ToolError::new(
                ToolErrorKind::Execution,
                "read: image size arithmetic overflow",
            )
        })?;
    if encoded_len > MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES {
        return Err(ToolError::new(
            ToolErrorKind::Execution,
            format!(
                "read: encoded image exceeds the {} byte safety limit",
                MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES
            ),
        ));
    }

    let mut reader = ImageReader::with_format(Cursor::new(raw), kind.format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| {
        ToolError::new(
            ToolErrorKind::Execution,
            format!(
                "read: cannot decode image {} within safety limits: {error}",
                path.display()
            ),
        )
    })?;
    let (width, height) = (image.width(), image.height());
    let data = base64::engine::general_purpose::STANDARD.encode(raw);
    Ok(vec![
        ToolContent::Text {
            text: format!(
                "Read image {} ({}, {width}x{height})",
                path.display(),
                kind.mime_type
            ),
        },
        ToolContent::Image {
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

pub fn read_runtime_tool(
    filesystem: FilesystemCapability,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    read_runtime_tool_with_operations(filesystem, Arc::new(RealReadOperations))
}

fn read_runtime_tool_with_operations(
    filesystem: FilesystemCapability,
    ops: Arc<dyn ReadOperations>,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("read").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<ReadArgs>().expect("ReadArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: true,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::WorkspaceLocalReadOnly,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<ReadArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            let ops = ops.clone();
            Box::pin(async move {
                let target = filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "read",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                read_target_with_operations(&target, args, ops).await
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

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
            [ToolContent::Text { text }, ToolContent::Image { data, mime_type }]
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
        assert!(error.message.contains("cannot decode image"));
    }

    #[test]
    fn typed_read_definition_matches_runtime_limits_and_capabilities() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let tool = read_runtime_tool(filesystem).unwrap();
        let definition = tool.definition();
        assert_eq!(definition.id.as_str(), "read");
        assert!(definition.capabilities.read_only);
        assert!(!definition.capabilities.provider_executed);
        assert_eq!(
            definition.authorization_risk,
            AuthorizationRisk::WorkspaceLocalReadOnly
        );
        assert_eq!(definition.parameters["additionalProperties"], false);
        assert_eq!(
            definition.parameters["properties"]["offset"]["anyOf"][0]["minimum"],
            1
        );
        assert_eq!(
            definition.parameters["properties"]["offset"]["anyOf"][0]["maximum"],
            MAX_LINE_ARGUMENT
        );
    }

    #[tokio::test]
    async fn typed_read_returns_content_and_revision_details() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("notes.txt"), "alpha\nbeta\n").expect("write fixture");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(read_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let context = ToolCallContext::new(
            ToolId::new("read").unwrap(),
            "read-call",
            CancellationToken::new(),
        );

        let output = runtime
            .execute(
                context,
                serde_json::json!({"path": "notes.txt", "offset": 2, "limit": 1}),
            )
            .await
            .expect("typed read succeeds");
        assert!(matches!(
            output.content.as_slice(),
            [ToolContent::Text { text }] if text == "beta\n\n[1 more lines in file. Use offset=3 to continue.]"
        ));
        let details = output.details.expect("read revision details");
        assert_eq!(details["path"], "notes.txt");
        assert_eq!(details["bytes"], 11);
        assert_eq!(
            details["content_sha256"],
            format!("{:x}", Sha256::digest(b"alpha\nbeta\n"))
        );
        assert!(
            details["target_fingerprint"]
                .as_str()
                .is_some_and(|fingerprint| fingerprint.len() == 64)
        );
        let anchors = details["hashline"].as_array().unwrap();
        assert_eq!(anchors.len(), 1);
        assert!(anchors[0].as_str().unwrap().starts_with("2:"));
        assert!(anchors[0].as_str().unwrap().contains("→beta"));
    }

    #[test]
    fn hashline_details_follow_the_requested_window_and_byte_budget() {
        let content = (1..=3_000)
            .map(|line| format!("{line:04}-{}", "x".repeat(80)))
            .collect::<Vec<_>>()
            .join("\n");
        let anchors = hashline_window(&content, 100, Some(3));
        assert_eq!(anchors.len(), 3);
        assert!(anchors[0].starts_with("100:"));
        assert!(
            anchors.iter().map(|anchor| anchor.len() + 1).sum::<usize>()
                <= crate::limits::MAX_HASHLINE_DETAILS_BYTES
        );

        let huge_line = "x".repeat(crate::limits::MAX_HASHLINE_DETAILS_BYTES + 1);
        assert!(hashline_window(&huge_line, 1, None).is_empty());
    }

    #[tokio::test]
    async fn typed_read_rejects_out_of_range_arguments_structurally() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("notes.txt"), "alpha").expect("write fixture");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(read_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let context = ToolCallContext::new(
            ToolId::new("read").unwrap(),
            "read-call",
            CancellationToken::new(),
        );

        let error = runtime
            .execute(
                context,
                serde_json::json!({"path": "notes.txt", "offset": 0}),
            )
            .await
            .expect_err("zero offset is invalid");
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
    }
}
