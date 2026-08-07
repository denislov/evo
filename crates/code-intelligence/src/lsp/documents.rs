//! LSP document 状态：open / change / close、版本跟踪、change 合并、
//! 重启后的 replay 列表。
//!
//! 设计决策（详见 `docs/refactor/phase8-lsp.md`）：
//!
//! - **本地保持完整文本**：任何 change 先在本地应用（合并同一版本的多批
//!   change），然后向服务器发**全量 didChange**（`range: null` + 最新
//!   文本）。LSP spec 明确支持全量同步，语义正确且让
//!   `DocumentStore` 与服务器文本严格一致（不依赖增量计算的正确性）。
//! - **版本单调**：`version < 当前版本` 拒绝（服务器视角文本已过期）；
//!   `version == 当前版本` 视为同批合并；`version > 当前版本` 接受并更新。
//! - **uri 校验**：必须 `file://` scheme 且解析后位于 workspace 内
//!   （路径逃逸拒绝，同 edit 层）。
//! - **replay 列表**：重启后按 uri 排序重发 didOpen + 最新文本。
//!
//! LSP 的 `position.character` 是 **UTF-16 code unit** 偏移（协议规定），
//! 本模块提供 UTF-16 ↔ char ↔ 字节 换算工具，range 应用与校验都用它。

// Evo 独立设计：无 Grok 参考（Grok 的 LSP client 不管理 document
// state——async-lsp 的 server 端才需要）；合并/全量重发与版本单调规则
// 为 Evo 自研。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// LSP `Position`（行 / 列都从 0 开始；`character` 是 UTF-16 code unit）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// LSP `Range`（半开区间，行 / 列从 0 开始）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// 一个文档的 uri（`file://` scheme + workspace 内绝对路径）。
///
/// 构造经 [`DocumentStore::open`] 校验；`abs_path` 是解析后的绝对路径
/// （不 canonicalize，避免对不存在文件做 IO；只做词法上的 workspace
/// 包含检查）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentUri {
    raw: String,
    abs_path: PathBuf,
}

impl DocumentUri {
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// 解析后的绝对路径。
    pub fn abs_path(&self) -> &Path {
        &self.abs_path
    }
}

/// 打开中的文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDocument {
    pub uri: DocumentUri,
    pub language_id: String,
    /// LSP 版本号（服务器与客户端同步用）。
    pub version: i64,
    pub text: String,
}

/// 一次文档内容变更（LSP `TextDocumentContentChangeEvent`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentChange {
    /// `None` = 全量替换。
    pub range: Option<Range>,
    pub text: String,
}

/// document 操作的错误分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("document uri is invalid: {detail}")]
    InvalidUri { detail: String },
    #[error("document uri is outside the workspace: {detail}")]
    OutsideWorkspace { detail: String },
    #[error("document {uri} is not open")]
    NotOpen { uri: String },
    #[error("document {uri} is already open (close it first)")]
    AlreadyOpen { uri: String },
    #[error("document change version {given} is older than current {current} for {uri}")]
    StaleVersion {
        uri: String,
        given: i64,
        current: i64,
    },
    #[error("range {range:?} is out of bounds for document {uri}")]
    RangeOutOfBounds { uri: String, range: Range },
}

/// workspace 内的打开文档集合（BTreeMap 按 uri 排序，replay 确定）。
#[derive(Debug, Clone)]
pub struct DocumentStore {
    workspace_root: PathBuf,
    documents: BTreeMap<DocumentUri, OpenDocument>,
}

impl DocumentStore {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            documents: BTreeMap::new(),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// 校验并构造 [`DocumentUri`]：`file://` scheme + 绝对路径 + workspace
    /// 内（词法包含检查，`..` 逃逸拒绝）。
    pub fn parse_uri(&self, raw: &str) -> Result<DocumentUri, DocumentError> {
        let parsed = raw
            .strip_prefix("file://")
            .ok_or_else(|| DocumentError::InvalidUri {
                detail: format!("{raw:?} does not use the file:// scheme"),
            })?;
        let path = PathBuf::from(parsed);
        if !path.is_absolute() {
            return Err(DocumentError::InvalidUri {
                detail: format!("{raw:?} is not an absolute path"),
            });
        }
        let within = lexically_within(&path, &self.workspace_root);
        if !within {
            return Err(DocumentError::OutsideWorkspace {
                detail: format!("{raw:?} is outside {}", self.workspace_root.display()),
            });
        }
        Ok(DocumentUri {
            raw: raw.to_string(),
            abs_path: path,
        })
    }

    /// 打开文档。重复 open 拒绝（必须先 close）。
    pub fn open(
        &mut self,
        uri: &str,
        language_id: &str,
        version: i64,
        text: &str,
    ) -> Result<DocumentUri, DocumentError> {
        let uri = self.parse_uri(uri)?;
        if self.documents.contains_key(&uri) {
            return Err(DocumentError::AlreadyOpen {
                uri: uri.raw.clone(),
            });
        }
        self.documents.insert(
            uri.clone(),
            OpenDocument {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version,
                text: text.to_string(),
            },
        );
        Ok(uri)
    }

    /// 应用一批内容变更并返回变更后的文档（版本单调校验 + 同版本合并）。
    pub fn change(
        &mut self,
        uri: &str,
        version: i64,
        changes: &[ContentChange],
    ) -> Result<OpenDocument, DocumentError> {
        let uri = self.parse_uri(uri)?;
        let current = self
            .documents
            .get(&uri)
            .ok_or_else(|| DocumentError::NotOpen {
                uri: uri.raw.clone(),
            })?;
        if version < current.version {
            return Err(DocumentError::StaleVersion {
                uri: uri.raw.clone(),
                given: version,
                current: current.version,
            });
        }
        let mut next = current.clone();
        next.version = version;
        for change in changes {
            next.text = apply_change(&next.text, change).map_err(|range| {
                DocumentError::RangeOutOfBounds {
                    uri: uri.raw.clone(),
                    range,
                }
            })?;
        }
        self.documents.insert(uri.clone(), next.clone());
        Ok(next)
    }

    /// 关闭文档并返回它（未知 uri 报错）。
    pub fn close(&mut self, uri: &str) -> Result<OpenDocument, DocumentError> {
        let uri = self.parse_uri(uri)?;
        self.documents
            .remove(&uri)
            .ok_or_else(|| DocumentError::NotOpen {
                uri: uri.raw.clone(),
            })
    }

    pub fn get(&self, uri: &str) -> Result<&OpenDocument, DocumentError> {
        let uri = self.parse_uri(uri)?;
        self.documents
            .get(&uri)
            .ok_or_else(|| DocumentError::NotOpen {
                uri: uri.raw.clone(),
            })
    }

    pub fn is_open(&self, uri: &str) -> bool {
        self.parse_uri(uri)
            .is_ok_and(|uri| self.documents.contains_key(&uri))
    }

    /// 打开文档数。
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// 重启后的 replay 列表（按 uri 排序，确定性）。
    pub fn replay_list(&self) -> Vec<OpenDocument> {
        self.documents.values().cloned().collect()
    }
}

/// 词法上检查 `path` 是否位于 `root` 之下：路径段前缀必须完全等于
/// `root` 的段，且剩余段不允许 `..`（会把路径拉回 root 或更上层）。
/// 不做 IO（文件可能不存在）。
fn lexically_within(path: &Path, root: &Path) -> bool {
    let root_components: Vec<_> = root.components().collect();
    let path_components: Vec<_> = path.components().collect();
    if path_components.len() < root_components.len() {
        return false;
    }
    if path_components[..root_components.len()] != root_components[..] {
        return false;
    }
    path_components[root_components.len()..]
        .iter()
        .all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

/// 把 UTF-16 code unit 偏移换算成 char 索引（LSP `character` → 本地索引）。
/// 越界返回 `text.chars().count()`（调用方再决定是否拒绝）。
pub fn utf16_to_char_index(text: &str, utf16_offset: usize) -> usize {
    let mut char_index = 0usize;
    let mut utf16_index = 0usize;
    for character in text.chars() {
        if utf16_index >= utf16_offset {
            break;
        }
        utf16_index += character.len_utf16();
        char_index += 1;
    }
    char_index
}

/// 把 char 索引换算成 UTF-16 code unit 偏移（本地索引 → LSP `character`）。
pub fn char_index_to_utf16(text: &str, char_index: usize) -> usize {
    text.chars().take(char_index).map(char::len_utf16).sum()
}

/// 把 LSP `Position` 转成**字节偏移**（行定位 + UTF-16 换算）。
fn position_to_char_index(text: &str, position: Position) -> Result<usize, Range> {
    let mut current_line = 0u32;
    let mut offset = 0usize;
    for (char_index, character) in text.char_indices() {
        if current_line == position.line {
            let line_text = &text[offset..];
            let line_char = line_text.chars().count();
            let target = utf16_to_char_index(line_text, position.character as usize);
            if target > line_char {
                return Err(Range {
                    start: position,
                    end: position,
                });
            }
            // char 索引 → 行内字节偏移（UTF-8 变长编码）。
            let byte_offset = line_text
                .char_indices()
                .nth(target)
                .map(|(byte, _)| byte)
                .unwrap_or(line_text.len());
            return Ok(char_index + byte_offset);
        }
        if character == '\n' {
            current_line += 1;
            offset = char_index + 1;
        }
    }
    // 文件末尾：允许 position 指向最后一行结束处。
    if current_line == position.line {
        let line_text = &text[offset..];
        let target = utf16_to_char_index(line_text, position.character as usize);
        if target <= line_text.chars().count() {
            let byte_offset = line_text
                .char_indices()
                .nth(target)
                .map(|(byte, _)| byte)
                .unwrap_or(line_text.len());
            return Ok(offset + byte_offset);
        }
    }
    Err(Range {
        start: position,
        end: position,
    })
}

/// 应用一个内容变更到全文（`range: None` = 全量替换）。
pub fn apply_change(text: &str, change: &ContentChange) -> Result<String, Range> {
    let Some(range) = change.range else {
        return Ok(change.text.clone());
    };
    let start = position_to_char_index(text, range.start)?;
    let end = position_to_char_index(text, range.end)?;
    if start > end {
        return Err(range);
    }
    let mut out = String::with_capacity(text.len() + change.text.len());
    out.push_str(&text[..start]);
    out.push_str(&change.text);
    out.push_str(&text[end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &Path) -> DocumentStore {
        DocumentStore::new(root.to_path_buf())
    }

    #[test]
    fn open_change_close_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        let parsed = store.open(&uri, "rust", 1, "fn main() {}\n").unwrap();
        assert_eq!(parsed.as_str(), uri);
        assert_eq!(store.len(), 1);
        assert!(store.is_open(&uri));

        let changed = store
            .change(
                &uri,
                2,
                &[ContentChange {
                    range: None,
                    text: "fn main() {}\nfn helper() {}\n".into(),
                }],
            )
            .unwrap();
        assert_eq!(changed.version, 2);
        assert!(changed.text.contains("helper"));

        let closed = store.close(&uri).unwrap();
        assert_eq!(closed.version, 2);
        assert!(store.is_empty());
    }

    #[test]
    fn duplicate_open_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&uri, "rust", 1, "x").unwrap();
        assert!(matches!(
            store.open(&uri, "rust", 2, "y"),
            Err(DocumentError::AlreadyOpen { .. })
        ));
    }

    #[test]
    fn stale_version_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&uri, "rust", 5, "x").unwrap();
        assert!(matches!(
            store.change(&uri, 4, &[]),
            Err(DocumentError::StaleVersion { .. })
        ));
    }

    #[test]
    fn same_version_merges_batches() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&uri, "rust", 1, "hello").unwrap();
        store
            .change(
                &uri,
                2,
                &[ContentChange {
                    range: Some(Range {
                        start: Position {
                            line: 0,
                            character: 5,
                        },
                        end: Position {
                            line: 0,
                            character: 5,
                        },
                    }),
                    text: " world".into(),
                }],
            )
            .unwrap();
        let merged = store
            .change(
                &uri,
                2,
                &[ContentChange {
                    range: Some(Range {
                        start: Position {
                            line: 0,
                            character: 11,
                        },
                        end: Position {
                            line: 0,
                            character: 11,
                        },
                    }),
                    text: "!".into(),
                }],
            )
            .unwrap();
        assert_eq!(merged.text, "hello world!");
    }

    #[test]
    fn incremental_range_apply_and_full_replace() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&uri, "rust", 1, "alpha\nbeta\ngamma\n").unwrap();
        // 替换第 1 行（0-indexed）的 beta。
        store
            .change(
                &uri,
                2,
                &[ContentChange {
                    range: Some(Range {
                        start: Position {
                            line: 1,
                            character: 0,
                        },
                        end: Position {
                            line: 1,
                            character: 4,
                        },
                    }),
                    text: "BETA".into(),
                }],
            )
            .unwrap();
        let doc = store.get(&uri).unwrap();
        assert_eq!(doc.text, "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn utf16_surrogate_pairs_are_counted_by_code_units() {
        // "a😀b"：😀 是代理对，UTF-16 长度 2。
        let text = "a😀b";
        assert_eq!(text.chars().count(), 3);
        assert_eq!(text.encode_utf16().count(), 4);
        assert_eq!(utf16_to_char_index(text, 0), 0);
        assert_eq!(utf16_to_char_index(text, 1), 1);
        assert_eq!(utf16_to_char_index(text, 3), 2);
        assert_eq!(utf16_to_char_index(text, 4), 3);
        assert_eq!(char_index_to_utf16(text, 2), 3);
        // 插入点位于代理对中间（character=2 是非法位置，落在 char 1 之后）。
        let applied = apply_change(
            text,
            &ContentChange {
                range: Some(Range {
                    start: Position {
                        line: 0,
                        character: 2,
                    },
                    end: Position {
                        line: 0,
                        character: 2,
                    },
                }),
                text: "X".into(),
            },
        )
        .unwrap();
        assert_eq!(applied, "a😀Xb");
    }

    #[test]
    fn out_of_bounds_range_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&uri, "rust", 1, "ab\n").unwrap();
        assert!(matches!(
            store.change(
                &uri,
                2,
                &[ContentChange {
                    range: Some(Range {
                        start: Position {
                            line: 5,
                            character: 0,
                        },
                        end: Position {
                            line: 5,
                            character: 1,
                        },
                    }),
                    text: "x".into(),
                }],
            ),
            Err(DocumentError::RangeOutOfBounds { .. })
        ));
    }

    #[test]
    fn uri_validation_rejects_non_file_and_escape() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(matches!(
            store.parse_uri("https://example.com/a.rs"),
            Err(DocumentError::InvalidUri { .. })
        ));
        assert!(
            store
                .parse_uri(&format!("file://{}/a.rs", temp.path().display()))
                .is_ok()
        );
        // 相对路径拒绝。
        assert!(matches!(
            store.parse_uri("file://a.rs"),
            Err(DocumentError::InvalidUri { .. })
        ));
        // 逃逸拒绝。
        let escaped = format!("file://{}/../outside.rs", temp.path().display());
        assert!(matches!(
            store.parse_uri(&escaped),
            Err(DocumentError::OutsideWorkspace { .. })
        ));
        // 绝对路径但不在 workspace 内。
        let elsewhere = "file:///tmp/not-in-workspace.rs";
        assert!(matches!(
            store.parse_uri(elsewhere),
            Err(DocumentError::OutsideWorkspace { .. })
        ));
    }

    #[test]
    fn replay_list_is_sorted_and_holds_latest_text() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let b_uri = format!("file://{}/b.rs", temp.path().display());
        let a_uri = format!("file://{}/a.rs", temp.path().display());
        store.open(&b_uri, "rust", 1, "b v1").unwrap();
        store.open(&a_uri, "rust", 1, "a v1").unwrap();
        store
            .change(
                &a_uri,
                2,
                &[ContentChange {
                    range: None,
                    text: "a v2".into(),
                }],
            )
            .unwrap();
        let replay = store.replay_list();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].uri.as_str(), a_uri);
        assert_eq!(replay[0].text, "a v2");
        assert_eq!(replay[0].version, 2);
        assert_eq!(replay[1].uri.as_str(), b_uri);
    }

    #[test]
    fn not_open_errors() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = store(temp.path());
        let uri = format!("file://{}/missing.rs", temp.path().display());
        assert!(matches!(
            store.get(&uri),
            Err(DocumentError::NotOpen { .. })
        ));
        assert!(matches!(
            store.close(&uri),
            Err(DocumentError::NotOpen { .. })
        ));
        assert!(matches!(
            store.change(&uri, 1, &[]),
            Err(DocumentError::NotOpen { .. })
        ));
    }
}
