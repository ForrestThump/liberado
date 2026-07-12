//! Content-keyed cache of parsed markdown (`Vec<MarkdownLine>`).
//!
//! Shared intermediate form (not ratatui `Line`s) so WebUI can reuse the same cache idea later.
//! Invalidation is automatic: keys are full source strings; new content = miss.

use std::collections::HashMap;
use std::sync::Arc;

use liberado_markdown::{MarkdownLine, markdown_to_lines};

/// Cap entries so a long session can't grow unbounded. Simple bulk-clear when full
/// (history scrolling usually reuses the same message bodies).
const MAX_ENTRIES: usize = 256;

/// Parse cache for assistant message bodies.
#[derive(Debug, Default)]
pub struct MarkdownParseCache {
    entries: HashMap<String, Arc<Vec<MarkdownLine>>>,
    hits: u64,
    misses: u64,
}

impl MarkdownParseCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return cached parse of `markdown`, or parse and store.
    pub fn get_or_parse(&mut self, markdown: &str) -> Arc<Vec<MarkdownLine>> {
        if let Some(hit) = self.entries.get(markdown) {
            self.hits = self.hits.saturating_add(1);
            return Arc::clone(hit);
        }
        self.misses = self.misses.saturating_add(1);
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.clear();
        }
        let parsed = Arc::new(markdown_to_lines(markdown));
        self.entries
            .insert(markdown.to_string(), Arc::clone(&parsed));
        parsed
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        // Keep hit/miss counters for profiling across clears.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_parse_is_cache_hit() {
        let mut cache = MarkdownParseCache::new();
        let a = cache.get_or_parse("**bold** and `code`");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
        let b = cache.get_or_parse("**bold** and `code`");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn different_content_is_miss() {
        let mut cache = MarkdownParseCache::new();
        let _ = cache.get_or_parse("hello");
        let _ = cache.get_or_parse("world");
        assert_eq!(cache.misses(), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn content_change_does_not_reuse_stale() {
        let mut cache = MarkdownParseCache::new();
        let first = cache.get_or_parse("# Title");
        let second = cache.get_or_parse("# Title\n\nmore");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(cache.misses(), 2);
    }
}
