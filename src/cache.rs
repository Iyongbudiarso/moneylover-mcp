use std::{
    future::Future,
    time::{Duration, Instant},
};

use tokio::sync::Mutex;

use crate::error::Result;

/// Whether a caller accepts cached data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Use the cache if it is still valid.
    Cached,
    /// Always ask Money Lover, then update the cache.
    Fresh,
}

/// Data plus whether it was served from the cache (and so may be out of date).
pub struct Loaded<T> {
    pub data: T,
    pub from_cache: bool,
}

impl<T> Loaded<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Loaded<U> {
        Loaded {
            data: f(self.data),
            from_cache: self.from_cache,
        }
    }
}

/// A single value kept for `ttl`. The lock is held while loading, so concurrent
/// callers wait for one request instead of all hitting the (slow) API.
pub struct TtlCache<T> {
    ttl: Duration,
    slot: Mutex<Option<(Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    /// A zero `ttl` disables caching.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slot: Mutex::new(None),
        }
    }

    pub async fn get<F, Fut>(&self, mode: Fetch, load: F) -> Result<Loaded<T>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let mut slot = self.slot.lock().await;
        if mode == Fetch::Cached
            && let Some((stored_at, value)) = slot.as_ref()
            && stored_at.elapsed() < self.ttl
        {
            return Ok(Loaded {
                data: value.clone(),
                from_cache: true,
            });
        }
        let value = load().await?;
        if !self.ttl.is_zero() {
            *slot = Some((Instant::now(), value.clone()));
        }
        Ok(Loaded {
            data: value,
            from_cache: false,
        })
    }

    pub async fn clear(&self) {
        *self.slot.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn get(cache: &TtlCache<usize>, loads: &AtomicUsize, mode: Fetch) -> (usize, bool) {
        let loaded = cache
            .get(mode, || async {
                Ok(loads.fetch_add(1, Ordering::SeqCst) + 1)
            })
            .await
            .unwrap();
        (loaded.data, loaded.from_cache)
    }

    #[tokio::test]
    async fn caches_until_fresh_is_requested_or_cleared() {
        let cache = TtlCache::new(Duration::from_secs(60));
        let loads = AtomicUsize::new(0);
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (1, false));
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (1, true));
        assert_eq!(get(&cache, &loads, Fetch::Fresh).await, (2, false));
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (2, true));
        cache.clear().await;
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (3, false));
    }

    #[tokio::test]
    async fn expires_and_zero_ttl_disables() {
        let cache = TtlCache::new(Duration::from_millis(20));
        let loads = AtomicUsize::new(0);
        get(&cache, &loads, Fetch::Cached).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (2, false));

        let disabled = TtlCache::new(Duration::ZERO);
        let loads = AtomicUsize::new(0);
        get(&disabled, &loads, Fetch::Cached).await;
        assert_eq!(get(&disabled, &loads, Fetch::Cached).await, (2, false));
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let cache: TtlCache<usize> = TtlCache::new(Duration::from_secs(60));
        let failed = cache
            .get(Fetch::Cached, || async {
                Err(crate::error::Error::Transient("x".into()))
            })
            .await;
        assert!(failed.is_err());
        let loads = AtomicUsize::new(0);
        assert_eq!(get(&cache, &loads, Fetch::Cached).await, (1, false));
    }
}
