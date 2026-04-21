//! In-memory cooldown tracker used for per-IP and per-address faucet limits.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tracks the last time a given key was served and answers "is this key in
/// cooldown right now?" queries.
///
/// State is process-local and resets on restart. That is acceptable for
/// vibenet, which wipes chain state on every redeploy anyway.
#[derive(Debug)]
pub struct Limiter<K: Eq + Hash> {
    inner: Mutex<HashMap<K, Instant>>,
}

impl<K: Eq + Hash + Clone> Limiter<K> {
    /// Create an empty limiter.
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    /// If `key` has not been served within `cooldown`, mark it served now and
    /// return `None`. Otherwise return the remaining duration before the next
    /// allowed request.
    pub fn try_acquire(&self, key: K, cooldown: Duration) -> Option<Duration> {
        let mut map = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();

        if let Some(last) = map.get(&key) {
            let elapsed = now.duration_since(*last);
            if elapsed < cooldown {
                return Some(cooldown - elapsed);
            }
        }

        map.insert(key, now);
        None
    }

    /// Undo a previous `try_acquire` for `key`. Used when the downstream
    /// action (e.g. sending a transaction) fails and we don't want to punish
    /// the user for our failure.
    pub fn release(&self, key: &K) {
        let mut map = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.remove(key);
    }
}

impl<K: Eq + Hash + Clone> Default for Limiter<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_applies_after_first_request() {
        let limiter = Limiter::<&'static str>::new();
        assert!(limiter.try_acquire("a", Duration::from_secs(60)).is_none());
        assert!(limiter.try_acquire("a", Duration::from_secs(60)).is_some());
    }

    #[test]
    fn release_clears_cooldown() {
        let limiter = Limiter::<&'static str>::new();
        assert!(limiter.try_acquire("a", Duration::from_secs(60)).is_none());
        limiter.release(&"a");
        assert!(limiter.try_acquire("a", Duration::from_secs(60)).is_none());
    }
}
