//! 查询工具的输出预算（ARC-830）：结果条数上限 + 内容字节上限。
//!
//! 每个 read-only 查询工具持有独立 [`QueryOutputBudget`]（默认 100 条 /
//! 64 KiB）；执行时对结果先按条数截断、再按序列化字节截断，任何一层
//! 截断都必须显式标记（`truncated`），**不静默截断**——模型必须知道
//! 结果被裁剪过。

/// 查询工具的输出预算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryOutputBudget {
    /// 结果条数上限（`0` = 不限）。
    pub max_items: usize,
    /// 内容字节上限（`0` = 不限；按序列化字节估算）。
    pub max_bytes: usize,
}

impl Default for QueryOutputBudget {
    fn default() -> Self {
        Self {
            max_items: 100,
            max_bytes: 64 * 1024,
        }
    }
}

/// 按条数预算截断：返回 `(保留的前缀, 是否截断)`。
pub fn truncate_items<T>(items: Vec<T>, max_items: usize) -> (Vec<T>, bool) {
    if max_items == 0 || items.len() <= max_items {
        return (items, false);
    }
    let truncated = true;
    let mut items = items;
    items.truncate(max_items);
    (items, truncated)
}

/// 按字节预算截断：逐条累加 `item_bytes`（序列化字节估算），超出即停。
/// 返回 `(保留的前缀, 是否截断, 保留内容的字节数)`。
pub fn truncate_by_bytes<T>(
    items: Vec<T>,
    max_bytes: usize,
    item_bytes: impl Fn(&T) -> usize,
) -> (Vec<T>, bool, usize) {
    if max_bytes == 0 {
        let bytes = items.iter().map(&item_bytes).sum();
        return (items, false, bytes);
    }
    let mut kept = Vec::new();
    let mut bytes = 0_usize;
    let mut truncated = false;
    for item in items {
        let extra = item_bytes(&item);
        if bytes + extra > max_bytes {
            truncated = true;
            break;
        }
        bytes += extra;
        kept.push(item);
    }
    (kept, truncated, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_bytes(item: &u64) -> usize {
        serde_json::to_vec(item).unwrap().len()
    }

    #[test]
    fn item_budget_keeps_prefix_and_marks() {
        let (kept, truncated) = truncate_items(vec![1, 2, 3, 4], 2);
        assert_eq!(kept, vec![1, 2]);
        assert!(truncated);
    }

    #[test]
    fn item_budget_within_limit_is_not_truncated() {
        let (kept, truncated) = truncate_items(vec![1, 2, 3], 10);
        assert_eq!(kept, vec![1, 2, 3]);
        assert!(!truncated);
    }

    #[test]
    fn zero_item_budget_means_unlimited() {
        let (kept, truncated) = truncate_items(vec![1, 2, 3], 0);
        assert_eq!(kept, vec![1, 2, 3]);
        assert!(!truncated);
    }

    #[test]
    fn byte_budget_keeps_prefix_and_reports_bytes() {
        // 每项序列化为 1 字节；预算 2 → 保留 2 项。
        let (kept, truncated, bytes) = truncate_by_bytes(vec![1u64, 2, 3, 4], 2, item_bytes);
        assert_eq!(kept, vec![1, 2]);
        assert!(truncated);
        assert_eq!(bytes, 2);
    }

    #[test]
    fn byte_budget_within_limit_is_not_truncated() {
        let (kept, truncated, bytes) = truncate_by_bytes(vec![1u64, 2], 100, item_bytes);
        assert_eq!(kept, vec![1, 2]);
        assert!(!truncated);
        assert_eq!(bytes, 2);
    }

    #[test]
    fn zero_byte_budget_means_unlimited() {
        let (kept, truncated, _) = truncate_by_bytes(vec![1u64, 2, 3], 0, item_bytes);
        assert_eq!(kept, vec![1, 2, 3]);
        assert!(!truncated);
    }

    #[test]
    fn first_item_over_budget_keeps_nothing() {
        let (kept, truncated, bytes) = truncate_by_bytes(vec![100u64], 1, item_bytes);
        assert!(kept.is_empty());
        assert!(truncated);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn default_budget_bounds_queries() {
        let budget = QueryOutputBudget::default();
        assert_eq!(budget.max_items, 100);
        assert_eq!(budget.max_bytes, 64 * 1024);
    }
}
