//! `Position` / `Range`：源码位置与范围类型。
//!
//! 内部按 tree-sitter 约定以 0-indexed 存储；对外（查询结果 / LSP / 展示）
//! 提供 1-indexed accessor。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (types/range.rs); trimmed to what the Evo query surface needs (byte offsets,
// line/column accessors, containment checks) — the position-index helpers and
// display conversions are dropped.
use serde::{Deserialize, Serialize};

/// 源码中的一个位置（0-indexed 存储，tree-sitter 兼容）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    /// 行号（0-indexed）。
    line: usize,
    /// 列号（0-indexed）。
    character: usize,
    /// 从文件头开始的字节偏移。
    byte_offset: usize,
}

impl Position {
    pub fn new(line: usize, character: usize, byte_offset: usize) -> Self {
        Self {
            line,
            character,
            byte_offset,
        }
    }

    /// 0-indexed 行号。
    pub fn line(&self) -> usize {
        self.line
    }

    /// 1-indexed 行号（展示 / LSP 输出）。
    pub fn line_1indexed(&self) -> usize {
        self.line + 1
    }

    /// 0-indexed 列号。
    pub fn column(&self) -> usize {
        self.character
    }

    /// 1-indexed 列号（展示 / LSP 输出）。
    pub fn column_1indexed(&self) -> usize {
        self.character + 1
    }

    /// 字节偏移。
    pub fn byte_offset(&self) -> usize {
        self.byte_offset
    }
}

/// 源码中的一个范围（0-indexed 存储）。
///
/// `byte_size()` 与 Grok 保持一致（含端点的差 + 1），用于「最紧包含」的
/// 排序比较。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    start_position: Position,
    end_position: Position,
}

impl Range {
    pub fn new(start_position: Position, end_position: Position) -> Self {
        Self {
            start_position,
            end_position,
        }
    }

    /// 从 tree-sitter 节点构造范围。
    pub fn for_tree_node(node: &tree_sitter::Node<'_>) -> Self {
        let range = node.range();
        Self {
            start_position: Position {
                line: range.start_point.row,
                character: range.start_point.column,
                byte_offset: range.start_byte,
            },
            end_position: Position {
                line: range.end_point.row,
                character: range.end_point.column,
                byte_offset: range.end_byte,
            },
        }
    }

    pub fn start_position(&self) -> Position {
        self.start_position
    }

    pub fn end_position(&self) -> Position {
        self.end_position
    }

    pub fn start_byte(&self) -> usize {
        self.start_position.byte_offset
    }

    pub fn end_byte(&self) -> usize {
        self.end_position.byte_offset
    }

    /// 0-indexed 起始行。
    pub fn start_line(&self) -> usize {
        self.start_position.line
    }

    /// 1-indexed 起始行。
    pub fn start_line_1indexed(&self) -> usize {
        self.start_position.line + 1
    }

    /// 1-indexed 起始列。
    pub fn start_column_1indexed(&self) -> usize {
        self.start_position.character + 1
    }

    /// 跨度字节数（含端点）。
    pub fn byte_size(&self) -> usize {
        self.end_byte().saturating_sub(self.start_byte()) + 1
    }

    /// 是否为空范围（零字节）。
    pub fn is_empty(&self) -> bool {
        self.end_byte() == self.start_byte()
    }

    /// 本范围是否包含另一个范围（含边界，按行 / 列比较）。
    pub fn contains(&self, other: &Range) -> bool {
        let start_ok = self.start_line() < other.start_line()
            || (self.start_line() == other.start_line()
                && self.start_position.character <= other.start_position.character);
        let end_ok = self.end_line() > other.end_line()
            || (self.end_line() == other.end_line()
                && self.end_position.character >= other.end_position.character);
        start_ok && end_ok
    }

    /// 本范围是否严格包含另一个范围（两个范围不同且边界不重合）。
    pub fn strictly_contains(&self, other: &Range) -> bool {
        self != other
            && self.start_byte() <= other.start_byte()
            && self.end_byte() >= other.end_byte()
    }

    /// 0-indexed 结束行。
    pub fn end_line(&self) -> usize {
        self.end_position.line
    }

    /// 0-indexed 起始列。
    pub fn start_column(&self) -> usize {
        self.start_position.character
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, col: usize, byte: usize) -> Position {
        Position::new(line, col, byte)
    }

    #[test]
    fn one_indexed_accessors() {
        let p = Position::new(2, 5, 30);
        assert_eq!(p.line(), 2);
        assert_eq!(p.line_1indexed(), 3);
        assert_eq!(p.column(), 5);
        assert_eq!(p.column_1indexed(), 6);
        assert_eq!(p.byte_offset(), 30);
    }

    #[test]
    fn range_contains_semantics() {
        let outer = Range::new(pos(0, 0, 0), pos(10, 0, 100));
        let inner = Range::new(pos(1, 2, 10), pos(3, 4, 40));
        assert!(outer.contains(&inner));
        assert!(!inner.contains(&outer));
        assert!(outer.contains(&outer));
        // 相同范围不是严格包含。
        assert!(!outer.strictly_contains(&outer));
        assert!(outer.strictly_contains(&inner));
    }

    #[test]
    fn range_byte_size_includes_endpoint() {
        let range = Range::new(pos(0, 0, 4), pos(0, 4, 8));
        assert_eq!(range.byte_size(), 5);
        assert_eq!(range.start_byte(), 4);
        assert_eq!(range.end_byte(), 8);
    }
}
