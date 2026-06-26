//! A tiny bounded LRU cache used for the in-process scan/extract memoization.
//!
//! The caches exist so a `scan` followed by `read`/`edit` of the same bytes
//! does not reparse the file. Without a bound they would grow without limit in
//! a long-running agent process, so this caps the entry count and evicts the
//! least-recently-used entry on overflow.

use std::collections::HashMap;

/// A fixed-capacity least-recently-used cache.
///
/// Keys are content hashes (cheap to clone `String`s). Recency is tracked with
/// a monotonic counter rather than an intrusive list: simpler, and the entry
/// counts here are small (tens), so the linear eviction scan is negligible.
pub struct LruCache<V> {
    capacity: usize,
    tick: u64,
    entries: HashMap<String, (u64, V)>,
}

impl<V> LruCache<V> {
    /// Create a cache holding at most `capacity` entries (minimum 1).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            tick: 0,
            entries: HashMap::new(),
        }
    }

    /// Fetch a clone-free reference, bumping the entry's recency.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        let (last, value) = self.entries.get_mut(key)?;
        *last = tick;
        Some(value)
    }

    /// Insert or replace a value, evicting the LRU entry if over capacity.
    pub fn insert(&mut self, key: String, value: V) {
        self.tick += 1;
        let tick = self.tick;
        self.entries.insert(key, (tick, value));
        if self.entries.len() > self.capacity {
            self.evict_one();
        }
    }

    /// Drop all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.tick = 0;
    }

    fn evict_one(&mut self) {
        if let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, (last, _))| *last)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let mut cache: LruCache<u32> = LruCache::new(2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        // Touch "a" so "b" becomes the LRU.
        assert_eq!(cache.get("a"), Some(&1));
        cache.insert("c".into(), 3);
        assert_eq!(cache.get("b"), None); // evicted
        assert_eq!(cache.get("a"), Some(&1));
        assert_eq!(cache.get("c"), Some(&3));
    }

    #[test]
    fn capacity_is_at_least_one() {
        let mut cache: LruCache<u32> = LruCache::new(0);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b"), Some(&2));
    }
}
