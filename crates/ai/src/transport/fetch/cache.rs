use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::convert::OutputFormat;
use super::fetch::FetchResult;

/// Cache policy for successful, untruncated fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheConfig {
    pub ttl: Duration,
    pub max_entries: usize,
    pub max_total_bytes: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            max_entries: 64,
            max_total_bytes: 16 * 1024 * 1024,
        }
    }
}

struct CacheEntry {
    result: FetchResult,
    bytes: usize,
    fetched_at: SystemTime,
}

#[derive(Default)]
pub struct FetchCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    config: Option<CacheConfig>,
}

impl FetchCache {
    pub fn new(config: Option<CacheConfig>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            config,
        }
    }

    pub fn get(&self, key: &str) -> Option<FetchResult> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config = self.config.as_ref()?;
        let entry = entries.get(key)?;
        if entry.fetched_at.elapsed().ok()? > config.ttl {
            entries.remove(key);
            return None;
        }
        Some(entry.result.clone())
    }

    /// Store a successful fetch. Oversized and truncated results are rejected
    /// by the caller, so a cached entry always represents the full page.
    pub fn put(&self, key: &str, result: FetchResult) {
        let Some(config) = self.config else {
            return;
        };
        let bytes = result.text.len();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing = entries.get(key).map(|entry| entry.bytes).unwrap_or(0);
        if bytes > config.max_total_bytes {
            return;
        }
        if bytes > existing {
            let total: usize = entries.values().map(|entry| entry.bytes).sum();
            let budget = config
                .max_total_bytes
                .saturating_sub(total)
                .saturating_add(existing);
            if bytes > budget {
                evict_oldest(&mut entries, budget, key);
            }
        }
        entries.insert(
            key.to_string(),
            CacheEntry {
                result,
                bytes,
                fetched_at: SystemTime::now(),
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

fn evict_oldest(entries: &mut HashMap<String, CacheEntry>, budget: usize, except: &str) {
    // Runs before the new entry is inserted, so the map may hold a single
    // eviction candidate; a `len > 1` guard would never evict that one.
    while !entries.is_empty() {
        let oldest = entries
            .iter()
            .filter(|(key, _)| key.as_str() != except)
            .min_by_key(|(_, entry)| entry.fetched_at);
        let Some((oldest_key, _oldest_entry)) = oldest else {
            return;
        };
        let oldest_key = oldest_key.clone();
        entries.remove(&oldest_key);
        let total: usize = entries.values().map(|entry| entry.bytes).sum();
        if total <= budget {
            return;
        }
    }
}

/// Cache key is the normalized URL plus the requested output format: the
/// same page converted differently must not reuse a projection.
pub fn cache_key(url: &str, format: OutputFormat) -> String {
    format!("{url}\n{}", format.name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::fetch::fetch::FetchResult;

    fn result(text: &str) -> FetchResult {
        FetchResult {
            text: text.to_string(),
            final_url: "http://example.com/".into(),
            content_type: "text/html".into(),
            truncated: false,
            from_cache: false,
            fetched_at: SystemTime::now(),
        }
    }

    #[test]
    fn hits_are_returned_and_miss_returns_none() {
        let cache = FetchCache::new(Some(CacheConfig::default()));
        cache.put("k1", result("hello"));
        assert_eq!(cache.get("k1").unwrap().text, "hello");
        assert!(cache.get("k2").is_none());
    }

    #[test]
    fn ttl_expiry_invalidates_entries() {
        let cache = FetchCache::new(Some(CacheConfig {
            ttl: Duration::from_millis(10),
            ..CacheConfig::default()
        }));
        cache.put("k1", result("hello"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(cache.get("k1").is_none());
        assert_eq!(cache.len(), 0, "expired entry is removed");
    }

    #[test]
    fn disabled_cache_never_holds_entries() {
        let cache = FetchCache::new(None);
        cache.put("k1", result("hello"));
        assert_eq!(cache.len(), 0);
        assert!(cache.get("k1").is_none());
    }

    #[test]
    fn entry_bytes_are_bounded_and_oldest_evicts_first() {
        let cache = FetchCache::new(Some(CacheConfig {
            ttl: Duration::from_secs(60),
            max_entries: 100,
            max_total_bytes: 30,
        }));
        cache.put("a", result(&"x".repeat(20)));
        cache.put("b", result(&"y".repeat(20)));
        assert_eq!(cache.len(), 1);
        assert!(cache.get("a").is_none(), "oldest entry evicted");
        assert!(cache.get("b").is_some());
    }

    #[test]
    fn oversized_single_entry_is_not_cached() {
        let cache = FetchCache::new(Some(CacheConfig {
            ttl: Duration::from_secs(60),
            max_entries: 100,
            max_total_bytes: 10,
        }));
        cache.put("a", result(&"x".repeat(20)));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_key_separates_formats() {
        assert_ne!(
            cache_key("http://example.com/", OutputFormat::Markdown),
            cache_key("http://example.com/", OutputFormat::Text)
        );
        assert_eq!(
            cache_key("http://example.com/", OutputFormat::Markdown),
            cache_key("http://example.com/", OutputFormat::Markdown)
        );
    }
}
