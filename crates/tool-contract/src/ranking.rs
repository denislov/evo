//! 结果排序接口（ARC-830）：graph 查询与 MCP tool search 共用的
//! 「与查询词相关度」排序契约。
//!
//! 只定义排序逻辑（打分 + 稳定排序 + 截断），**不共享存储实现**——
//! graph 的符号索引与 MCP 的工具目录各自持有数据，各自调用本接口做
//! 结果排序。
//!
//! 契约：
//!
//! - [`RelevanceScorer::score`]：查询词与单条结果文本的相关度
//!   （`0.0` 不相关 ~ `1.0` 完全相关）；
//! - [`ResultRanker::rank`]：按相关度降序稳定排序（同分保持输入顺序，
//!   Rust `sort_by` 稳定语义），`limit` 截断（`0` = 不限）；
//! - 空查询词对所有结果打 `0.0` 分：全等分 → 保持输入顺序（列表语义）。
//!
//! 两侧调用方（Evo 自研，无上游参考）：
//!
//! - `code-intelligence`：符号搜索（`graph/query.rs::search_symbols`）；
//! - `extension-host`：`mcp_search` 命中结果排序（`mcp/meta.rs`）。
/// 排序结果条目：原始条目 + 相关度分数。
#[derive(Debug, Clone, PartialEq)]
pub struct RankedResult<T> {
    pub item: T,
    /// 相关度分数（`0.0` ~ `1.0`）。
    pub score: f64,
}

/// 相关度打分器：查询词与结果文本的匹配度。
pub trait RelevanceScorer: Send + Sync {
    fn score(&self, query: &str, text: &str) -> f64;
}

/// 默认打分器：大小写不敏感 token 匹配 + 前缀 / 子串降级匹配。
///
/// - 空查询词 → `0.0`（列表语义，顺序保持）；
/// - 文本与查询词完全相等 → `1.0`；
/// - 每个查询词 token 按匹配强度计分：整词命中 `0.8`、词前缀命中
///   `0.4`、词内子串命中 `0.2`，除以查询词 token 总数，封顶 `1.0`。
///   精确匹配高于整词命中（同名符号应排在同名包含项之前）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenOverlapScorer;

impl TokenOverlapScorer {
    pub fn new() -> Self {
        Self
    }
}

impl RelevanceScorer for TokenOverlapScorer {
    fn score(&self, query: &str, text: &str) -> f64 {
        let query = query.to_lowercase();
        let text = text.to_lowercase();
        if query.is_empty() {
            return 0.0;
        }
        if text == query {
            return 1.0;
        }
        let query_tokens = tokenize(&query);
        let text_tokens: Vec<&str> = tokenize(&text);
        if query_tokens.is_empty() || text_tokens.is_empty() {
            return 0.0;
        }
        let mut score = 0.0_f64;
        for query_token in &query_tokens {
            let exact = text_tokens.iter().any(|token| token == query_token);
            let prefix = !exact
                && text_tokens
                    .iter()
                    .any(|token| token.starts_with(query_token));
            let substring =
                !exact && !prefix && text_tokens.iter().any(|token| token.contains(query_token));
            score += if exact {
                0.8
            } else if prefix {
                0.4
            } else if substring {
                0.2
            } else {
                0.0
            };
        }
        (score / query_tokens.len() as f64).min(1.0)
    }
}

/// 结果排序接口：按与查询词的相关度降序稳定排序。
pub trait ResultRanker: Send + Sync {
    /// 排序并截断。`text_of` 给出每个结果的相关度文本；`limit` 为
    /// 结果条数上限（`0` = 不限）。同分保持输入顺序（稳定）。
    fn rank<T>(
        &self,
        query: &str,
        items: Vec<T>,
        text_of: impl Fn(&T) -> String,
        limit: usize,
    ) -> Vec<RankedResult<T>>;
}

/// 默认排序器：`TokenOverlapScorer` 打分 + 稳定降序排序 + `limit` 截断。
pub struct DefaultResultRanker {
    scorer: Box<dyn RelevanceScorer>,
}

impl Default for DefaultResultRanker {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultResultRanker {
    pub fn new() -> Self {
        Self {
            scorer: Box::new(TokenOverlapScorer::new()),
        }
    }

    pub fn with_scorer(scorer: impl RelevanceScorer + 'static) -> Self {
        Self {
            scorer: Box::new(scorer),
        }
    }
}

impl ResultRanker for DefaultResultRanker {
    fn rank<T>(
        &self,
        query: &str,
        items: Vec<T>,
        text_of: impl Fn(&T) -> String,
        limit: usize,
    ) -> Vec<RankedResult<T>> {
        let mut ranked: Vec<RankedResult<T>> = items
            .into_iter()
            .map(|item| {
                let score = self.scorer.score(query, &text_of(&item));
                RankedResult { item, score }
            })
            .collect();
        // `sort_by` 是稳定排序：同分保持输入顺序。
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if limit == 0 || ranked.len() <= limit {
            ranked
        } else {
            ranked.truncate(limit);
            ranked
        }
    }
}

/// 把文本切分为小写 token（ASCII 字母数字连续段）。
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, byte) in text.bytes().enumerate() {
        let is_token = byte.is_ascii_alphanumeric();
        match (start, is_token) {
            (None, true) => start = Some(index),
            (Some(_), false) => {
                if let Some(begin) = start.take() {
                    tokens.push(&text[begin..index]);
                }
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        tokens.push(&text[begin..]);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked_names<'a>(results: &[RankedResult<&'a str>]) -> Vec<&'a str> {
        results.iter().map(|result| result.item).collect()
    }

    #[test]
    fn tokenize_splits_on_punctuation_and_space() {
        assert_eq!(tokenize("foo_bar baz"), vec!["foo", "bar", "baz"]);
        assert_eq!(tokenize("a.b/c"), vec!["a", "b", "c"]);
        assert_eq!(tokenize("!!!"), Vec::<&str>::new());
        assert_eq!(tokenize(""), Vec::<&str>::new());
    }

    #[test]
    fn empty_query_returns_stable_input_order() {
        let ranker = DefaultResultRanker::new();
        let items = vec!["zeta", "alpha", "beta"];
        let ranked = ranker.rank("", items, |item| item.to_string(), 0);
        assert_eq!(ranked_names(&ranked), ["zeta", "alpha", "beta"]);
        assert!(ranked.iter().all(|result| result.score == 0.0));
    }

    #[test]
    fn empty_results_return_empty() {
        let ranker = DefaultResultRanker::new();
        let ranked = ranker.rank::<&str>("query", Vec::new(), |item| item.to_string(), 0);
        assert!(ranked.is_empty());
    }

    #[test]
    fn ranking_is_descending_by_relevance() {
        let ranker = DefaultResultRanker::new();
        let items = vec![
            "parse and index the codebase",
            "unrelated network helper",
            "index parse service",
            "index",
        ];
        let ranked = ranker.rank("index", items, |item| item.to_string(), 0);
        let scores: Vec<f64> = ranked.iter().map(|result| result.score).collect();
        assert!(scores.windows(2).all(|pair| pair[0] >= pair[1]));
        assert_eq!(ranked[0].item, "index");
        assert_eq!(ranked[0].score, 1.0);
    }

    #[test]
    fn equal_scores_keep_input_order() {
        let ranker = DefaultResultRanker::new();
        // 都不匹配 → 全 0 分；输入顺序必须保持。
        let items = vec!["first", "second", "third"];
        let ranked = ranker.rank("zzz", items, |item| item.to_string(), 0);
        assert_eq!(ranked_names(&ranked), ["first", "second", "third"]);
        assert!(ranked.iter().all(|result| result.score == 0.0));
    }

    #[test]
    fn limit_truncates_from_the_top() {
        let ranker = DefaultResultRanker::new();
        let items = vec!["index", "indexer", "indexing tool", "other"];
        let ranked = ranker.rank("index", items, |item| item.to_string(), 2);
        assert_eq!(ranked.len(), 2);
        // 截断只保留分数最高者。
        assert!(ranked[0].score >= ranked[1].score);
    }

    #[test]
    fn scoring_is_case_insensitive_with_prefix_bonus() {
        let scorer = TokenOverlapScorer::new();
        assert_eq!(scorer.score("INDEX", "index"), 1.0);
        assert!(scorer.score("index", "indexing service") > 0.0);
        assert!(scorer.score("parse", "parseable") > scorer.score("parse", "unrelated"));
        assert_eq!(scorer.score("", "anything"), 0.0);
    }

    #[test]
    fn custom_scorer_is_honored() {
        #[derive(Clone)]
        struct IdentityScorer;
        impl RelevanceScorer for IdentityScorer {
            fn score(&self, _query: &str, _text: &str) -> f64 {
                1.0
            }
        }
        let ranker = DefaultResultRanker::with_scorer(IdentityScorer);
        let ranked = ranker.rank("anything", vec!["a", "b"], |item| item.to_string(), 0);
        assert!(ranked.iter().all(|result| result.score == 1.0));
        assert_eq!(ranked_names(&ranked), ["a", "b"]);
    }
}
