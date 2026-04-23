//! Endpoint pool with active health probing and runtime failover.
//!
//! [`EndpointPool`] holds N RPC providers with stable indices and an atomic
//! active-index. Callers route every RPC call through [`active`](EndpointPool::active),
//! which returns the currently-selected provider. [`HealthMonitor`] probes
//! the active endpoint on a fixed interval and rotates the active index to
//! the first healthy alternative when the current one stops responding.
//!
//! Matches op-batcher's `dial.L2EndpointProvider`, where the active sequencer
//! is selected from a comma-separated list and re-checked every
//! `--active-sequencer-check-duration` (default 5s).

use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

/// Default circuit-breaker threshold: tolerate one transient transport error
/// and rotate the active endpoint on the second consecutive failure.
const DEFAULT_ROTATE_THRESHOLD: u32 = 2;

/// A pool of RPC endpoint providers with runtime active-endpoint tracking.
///
/// Holds an ordered list of providers and exposes a single "active" provider
/// via [`active`](Self::active). The active index is updated atomically by
/// [`HealthMonitor`] (or by callers via [`set_active`](Self::set_active)) so
/// that in-flight callers see a consistent provider for any one call but new
/// calls pick up the latest selection without locking.
///
/// The pool's circuit-breaker (`record_call_failure` / `record_call_success`)
/// packs `(active_idx, consecutive_failures)` into a single [`AtomicU64`] and
/// uses CAS loops for all mutations, so concurrent callers on the same pool
/// are safe without external synchronization.
#[derive(Debug)]
pub struct EndpointPool<P: ?Sized> {
    endpoints: Vec<EndpointEntry<P>>,
    /// Packed `(active_idx: u32, consecutive_failures: u32)`.
    state: AtomicU64,
    /// Consecutive call-failures before the circuit breaker rotates.
    rotate_threshold: u32,
}

#[derive(Debug)]
struct EndpointEntry<P: ?Sized> {
    url: String,
    provider: Arc<P>,
}

const fn pack(active: u32, failures: u32) -> u64 {
    (active as u64) << 32 | failures as u64
}

const fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

impl<P: ?Sized> EndpointPool<P> {
    /// Build a pool from one or more `(url, provider)` pairs.
    ///
    /// The first entry is the initial active endpoint. The circuit-breaker
    /// threshold defaults to 2 (tolerate one error, rotate on the second); override
    /// with [`with_rotate_threshold`](Self::with_rotate_threshold).
    /// Returns an error if the list is empty.
    pub fn new(endpoints: Vec<(Url, Arc<P>)>) -> eyre::Result<Self> {
        if endpoints.is_empty() {
            eyre::bail!("EndpointPool requires at least one endpoint");
        }
        let endpoints = endpoints
            .into_iter()
            .map(|(url, provider)| EndpointEntry { url: url.to_string(), provider })
            .collect();
        Ok(Self { endpoints, state: AtomicU64::new(0), rotate_threshold: DEFAULT_ROTATE_THRESHOLD })
    }

    /// Override the circuit-breaker rotate threshold.
    ///
    /// `threshold = 1` rotates immediately on every failure; `threshold = 2`
    /// tolerates one error and rotates on the second consecutive, etc.
    pub const fn with_rotate_threshold(mut self, threshold: u32) -> Self {
        self.rotate_threshold = threshold;
        self
    }

    /// Returns the currently-active provider.
    pub fn active(&self) -> Arc<P> {
        Arc::clone(&self.endpoints[self.active_index()].provider)
    }

    /// Returns the index of the currently-active endpoint.
    pub fn active_index(&self) -> usize {
        let (active, _) = unpack(self.state.load(Ordering::Acquire));
        active as usize
    }

    /// Returns the URL of the endpoint at `idx`.
    pub fn url_at(&self, idx: usize) -> &str {
        &self.endpoints[idx].url
    }

    /// Returns the provider at `idx`.
    pub fn provider_at(&self, idx: usize) -> Arc<P> {
        Arc::clone(&self.endpoints[idx].provider)
    }

    /// Number of endpoints in the pool.
    ///
    /// Always non-zero — [`new`](Self::new) rejects empty input.
    #[expect(clippy::len_without_is_empty, reason = "non-empty by construction")]
    pub const fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Switch the active endpoint to `idx` and reset the failure counter.
    /// Returns `true` if the active endpoint changed.
    pub fn set_active(&self, idx: usize) -> bool {
        assert!(idx < self.endpoints.len(), "endpoint index {idx} out of bounds");
        let new_state = pack(idx as u32, 0);
        loop {
            let current = self.state.load(Ordering::Acquire);
            let (prev_active, _) = unpack(current);
            if self
                .state
                .compare_exchange_weak(current, new_state, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return prev_active as usize != idx;
            }
        }
    }

    /// Record a successful call against the active endpoint. Clears the
    /// consecutive-failure counter so a single subsequent error doesn't
    /// trigger a circuit-breaker rotate.
    pub fn record_call_success(&self) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            let (active, failures) = unpack(current);
            if failures == 0 {
                return;
            }
            let new_state = pack(active, 0);
            if self
                .state
                .compare_exchange_weak(current, new_state, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Record a failed call against the active endpoint. When the
    /// consecutive failure count reaches the pool's
    /// [`rotate_threshold`](Self::with_rotate_threshold), rotates the
    /// active endpoint forward. No-op for single-endpoint pools.
    pub fn record_call_failure(&self) {
        let len = self.endpoints.len();
        if len <= 1 {
            return;
        }
        // Snapshot the active index at call time. If a concurrent rotation
        // changes the active endpoint between our load and CAS, we discard
        // the failure rather than penalising the newly-active endpoint
        // with a stale error count.
        let expected_active = self.active_index() as u32;
        loop {
            let current = self.state.load(Ordering::Acquire);
            let (active, failures) = unpack(current);
            if active != expected_active {
                return;
            }
            let new_failures = failures + 1;
            let new_state = if new_failures >= self.rotate_threshold {
                let next = (active as usize + 1) % len;
                pack(next as u32, 0)
            } else {
                pack(active, new_failures)
            };
            if self
                .state
                .compare_exchange_weak(current, new_state, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

/// Probe closure type — given an endpoint's pool index and provider, returns
/// a future resolving to `Ok(())` if the endpoint is healthy or
/// `Err(reason)` if the pool should consider failing over.
///
/// Stateful probes (e.g. head-advancement detection) capture their state in
/// the closure via [`Arc`]; stateless probes (`get_block_number` liveness)
/// can ignore the index. The future must be `'static` and `Send` because the
/// monitor invokes it from a background task.
type ProbeFn<P> =
    dyn Fn(usize, Arc<P>) -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static;

/// Background health monitor for an [`EndpointPool`].
///
/// On each tick of `interval`, runs the configured probe against the
/// currently-active endpoint. If the probe fails, walks the pool in index
/// order and switches the active index to the first endpoint whose probe
/// succeeds. If no endpoint is healthy, the active selection is left
/// unchanged so the next real RPC call still has a target — operators see
/// the failure as RPC errors with the last-known-good endpoint URL.
#[derive(derive_more::Debug)]
pub struct HealthMonitor<P: ?Sized> {
    pool: Arc<EndpointPool<P>>,
    interval: Duration,
    #[debug(skip)]
    probe: Arc<ProbeFn<P>>,
    label: &'static str,
}

impl<P: ?Sized + Send + Sync + 'static> HealthMonitor<P> {
    /// Create a new [`HealthMonitor`].
    ///
    /// `label` is included in log lines (e.g. `"l2-rpc"`, `"l1-rpc"`,
    /// `"rollup-rpc"`) so operators can attribute failover events to a
    /// specific pool. `probe` is the closure invoked on each tick — see
    /// [`Probe::block_number`] for the standard liveness check or
    /// [`Probe::head_advancement`] for the stateful variant.
    pub fn new<F>(
        pool: Arc<EndpointPool<P>>,
        interval: Duration,
        label: &'static str,
        probe: F,
    ) -> Self
    where
        F: Fn(usize, Arc<P>) -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static,
    {
        Self { pool, interval, probe: Arc::new(probe), label }
    }

    /// Run the monitor loop until `token` fires.
    ///
    /// Single-endpoint pools (`pool.len() == 1`) return immediately — there
    /// is nothing to fail over to, and the periodic probe would only generate
    /// noise.
    pub async fn run(self, token: CancellationToken) {
        if self.pool.len() <= 1 {
            debug!(label = %self.label, "single-endpoint pool, skipping health monitor");
            return;
        }
        info!(
            label = %self.label,
            endpoints = self.pool.len(),
            interval_secs = self.interval.as_secs(),
            "starting endpoint health monitor"
        );
        let mut ticker = tokio::time::interval(self.interval.max(Duration::from_millis(100)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick — the active endpoint was already
        // probed at startup by `connect_first`, so an extra probe in the
        // first millisecond just adds noise.
        ticker.tick().await;
        loop {
            tokio::select! {
                () = token.cancelled() => {
                    info!(label = %self.label, "endpoint health monitor stopping");
                    return;
                }
                _ = ticker.tick() => {}
            }
            self.check_once().await;
        }
    }

    /// Spawn [`run`](Self::run) on a tokio task tied to `token`.
    pub fn spawn(self, token: CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run(token))
    }

    /// Run a single probe cycle. Public so tests can drive the monitor
    /// deterministically without waiting on the timer.
    pub async fn check_once(&self) {
        let active_idx = self.pool.active_index();
        let active_provider = self.pool.provider_at(active_idx);
        let active_url = self.pool.url_at(active_idx).to_string();

        match (self.probe)(active_idx, active_provider).await {
            Ok(()) => {
                debug!(label = %self.label, url = %active_url, "active endpoint healthy");
            }
            Err(e) => {
                warn!(
                    label = %self.label,
                    url = %active_url,
                    error = %e,
                    "active endpoint failed health check, probing alternatives"
                );
                self.try_failover(active_idx, &active_url).await;
            }
        }
    }

    async fn try_failover(&self, current_idx: usize, current_url: &str) {
        for idx in 0..self.pool.len() {
            if idx == current_idx {
                continue;
            }
            let provider = self.pool.provider_at(idx);
            let url = self.pool.url_at(idx).to_string();
            match (self.probe)(idx, provider).await {
                Ok(()) => {
                    info!(
                        label = %self.label,
                        from = %current_url,
                        to = %url,
                        "active endpoint failover"
                    );
                    self.pool.set_active(idx);
                    return;
                }
                Err(e) => {
                    warn!(
                        label = %self.label,
                        url = %url,
                        error = %e,
                        "candidate endpoint also unhealthy"
                    );
                }
            }
        }
        warn!(label = %self.label, "no healthy endpoint available, keeping current selection");
    }
}

/// Namespace for probe-closure factories used by [`HealthMonitor`]. Grouping
/// the factories under a unit struct keeps the public API exporting a type
/// rather than a handful of loose functions.
#[derive(Debug)]
pub struct Probe;

impl Probe {
    /// Build a stateless liveness probe that calls `eth_blockNumber`.
    ///
    /// Ignores the pool index and reports the endpoint healthy whenever the
    /// call returns without a transport error. Does not detect the case
    /// where a passive read-replica or paused sequencer responds
    /// successfully without advancing the head — use
    /// [`Probe::head_advancement`] for that.
    ///
    /// Network must be specified at the call site:
    /// ```ignore
    /// Probe::block_number::<_, Ethereum>()
    /// ```
    pub fn block_number<P, N>()
    -> impl Fn(usize, Arc<P>) -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static
    where
        N: alloy_provider::Network,
        P: alloy_provider::Provider<N> + ?Sized + Send + Sync + 'static,
    {
        |_idx, provider| {
            Box::pin(async move {
                provider.get_block_number().await.map(|_| ()).map_err(|e| e.to_string())
            })
        }
    }

    /// Build a stateful probe that flags an endpoint unhealthy after
    /// `max_stalls` consecutive ticks where the head fails to advance.
    ///
    /// Catches the failure mode where a sequencer is paused or a node is a
    /// passive replica: `eth_blockNumber` returns successfully but the chain
    /// is not progressing. Per-endpoint state is keyed by pool index and
    /// lives in an `Arc<Mutex<_>>` captured by the returned closure. The
    /// first observation per endpoint primes state and always returns
    /// `Ok(())`, so the stall counter only starts ticking after the second
    /// probe.
    ///
    /// With `max_stalls = 2` and a 5s probe interval, the probe tolerates 5s
    /// of stall (one missed advance) and flags on the 10s mark — appropriate
    /// for an L2 with a 2s block time. Network must be specified at the call
    /// site:
    /// ```ignore
    /// Probe::head_advancement::<_, Base>(2)
    /// ```
    pub fn head_advancement<P, N>(
        max_stalls: u32,
    ) -> impl Fn(usize, Arc<P>) -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static
    where
        N: alloy_provider::Network,
        P: alloy_provider::Provider<N> + ?Sized + Send + Sync + 'static,
    {
        let state: Arc<Mutex<HashMap<usize, StallTracker>>> = Arc::new(Mutex::new(HashMap::new()));
        move |idx, provider| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let block = provider.get_block_number().await.map_err(|e| e.to_string())?;
                let mut state = state.lock().expect("stall-tracker mutex poisoned");
                StallTracker::prime_or_record(&mut state, idx, block, max_stalls)
            })
        }
    }
}

/// Per-endpoint state for [`Probe::head_advancement`]: the last block height
/// observed and the count of consecutive ticks since it advanced.
#[derive(Debug, Clone, Copy)]
struct StallTracker {
    last_block: u64,
    stalls: u32,
}

impl StallTracker {
    /// Record a new block observation. Returns `Ok(())` while the head
    /// advances or while the stall count is within tolerance; returns
    /// `Err` once the consecutive-stall count reaches `max_stalls`.
    fn record(&mut self, block: u64, max_stalls: u32) -> Result<(), String> {
        if block > self.last_block {
            self.last_block = block;
            self.stalls = 0;
            Ok(())
        } else {
            self.stalls = self.stalls.saturating_add(1);
            if self.stalls >= max_stalls {
                Err(format!(
                    "head not advancing for {} consecutive ticks (stuck at block {block})",
                    self.stalls
                ))
            } else {
                Ok(())
            }
        }
    }

    /// Prime or record an observation against per-endpoint state.
    ///
    /// First observation for a given `idx` primes state and returns `Ok`.
    /// Subsequent observations delegate to [`record`](Self::record). This
    /// is the shared implementation used by both [`Probe::head_advancement`]
    /// and its unit tests — change this function, not an inline copy.
    fn prime_or_record(
        state: &mut HashMap<usize, Self>,
        idx: usize,
        block: u64,
        max_stalls: u32,
    ) -> Result<(), String> {
        match state.entry(idx) {
            Entry::Vacant(slot) => {
                slot.insert(Self { last_block: block, stalls: 0 });
                Ok(())
            }
            Entry::Occupied(mut slot) => slot.get_mut().record(block, max_stalls),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use futures::FutureExt;

    use super::*;

    /// Stand-in "provider" used in unit tests. The pool's generic parameter
    /// is `?Sized` so we can also use it with concrete (sized) types.
    #[derive(Debug)]
    struct FakeProvider {
        healthy: AtomicBool,
        probe_count: AtomicU64,
    }

    impl FakeProvider {
        fn healthy() -> Arc<Self> {
            Arc::new(Self { healthy: AtomicBool::new(true), probe_count: AtomicU64::new(0) })
        }

        fn unhealthy() -> Arc<Self> {
            Arc::new(Self { healthy: AtomicBool::new(false), probe_count: AtomicU64::new(0) })
        }

        fn set_healthy(&self, h: bool) {
            self.healthy.store(h, Ordering::SeqCst);
        }

        fn probe_count(&self) -> u64 {
            self.probe_count.load(Ordering::SeqCst)
        }
    }

    fn fake_probe() -> impl Fn(usize, Arc<FakeProvider>) -> BoxFuture<'static, Result<(), String>>
    + Send
    + Sync
    + 'static {
        |_idx, p: Arc<FakeProvider>| {
            async move {
                p.probe_count.fetch_add(1, Ordering::SeqCst);
                if p.healthy.load(Ordering::SeqCst) { Ok(()) } else { Err("unhealthy".to_string()) }
            }
            .boxed()
        }
    }

    fn url(s: &str) -> Url {
        s.parse().expect("valid url in test")
    }

    fn pool_of(entries: Vec<(&str, Arc<FakeProvider>)>) -> Arc<EndpointPool<FakeProvider>> {
        Arc::new(
            EndpointPool::new(entries.into_iter().map(|(u, p)| (url(u), p)).collect()).unwrap(),
        )
    }

    fn monitor(pool: Arc<EndpointPool<FakeProvider>>) -> HealthMonitor<FakeProvider> {
        HealthMonitor::new(pool, Duration::from_secs(1), "test", fake_probe())
    }

    #[test]
    fn new_rejects_empty() {
        let pool: Result<EndpointPool<FakeProvider>, _> = EndpointPool::new(vec![]);
        assert!(pool.is_err());
    }

    #[test]
    fn set_active_swaps_endpoint() {
        let a = FakeProvider::healthy();
        let b = FakeProvider::healthy();
        let pool =
            pool_of(vec![("http://a:1234", Arc::clone(&a)), ("http://b:1234", Arc::clone(&b))]);

        assert_eq!(pool.active_index(), 0);
        assert!(Arc::ptr_eq(&pool.active(), &a));

        let changed = pool.set_active(1);
        assert!(changed);
        assert_eq!(pool.active_index(), 1);
        assert!(Arc::ptr_eq(&pool.active(), &b));

        let changed_again = pool.set_active(1);
        assert!(!changed_again, "swapping to the same index reports no change");
    }

    fn pool_with_threshold(
        entries: Vec<(&str, Arc<FakeProvider>)>,
        threshold: u32,
    ) -> Arc<EndpointPool<FakeProvider>> {
        Arc::new(
            EndpointPool::new(entries.into_iter().map(|(u, p)| (url(u), p)).collect())
                .unwrap()
                .with_rotate_threshold(threshold),
        )
    }

    #[test]
    fn record_call_failure_circuit_breaker_thresholds() {
        let a = FakeProvider::healthy();
        let b = FakeProvider::healthy();
        let c = FakeProvider::healthy();
        let pool = pool_with_threshold(
            vec![
                ("http://a:1234", Arc::clone(&a)),
                ("http://b:1234", Arc::clone(&b)),
                ("http://c:1234", Arc::clone(&c)),
            ],
            1,
        );

        // threshold=1 rotates immediately on every failure.
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 1);
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 2);
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 0, "rotation wraps around");
    }

    #[test]
    fn record_call_failure_tolerates_below_threshold() {
        let a = FakeProvider::healthy();
        let b = FakeProvider::healthy();
        let pool =
            pool_of(vec![("http://a:1234", Arc::clone(&a)), ("http://b:1234", Arc::clone(&b))]);

        // default threshold=2: first failure tolerated, second rotates.
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 0, "first failure within tolerance");
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 1, "second consecutive failure trips the breaker");
    }

    #[test]
    fn record_call_success_resets_failure_counter() {
        let a = FakeProvider::healthy();
        let b = FakeProvider::healthy();
        let pool =
            pool_of(vec![("http://a:1234", Arc::clone(&a)), ("http://b:1234", Arc::clone(&b))]);

        pool.record_call_failure();
        pool.record_call_success();
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 0, "counter reset by intervening success");
    }

    #[test]
    fn record_call_failure_noop_on_single_endpoint() {
        let p = FakeProvider::healthy();
        let pool = pool_with_threshold(vec![("http://only:1234", Arc::clone(&p))], 1);
        pool.record_call_failure();
        assert_eq!(pool.active_index(), 0, "no rotation possible with one endpoint");
    }

    #[tokio::test]
    async fn monitor_failover_when_active_unhealthy() {
        let bad = FakeProvider::unhealthy();
        let good = FakeProvider::healthy();
        let pool = pool_of(vec![
            ("http://bad:1234", Arc::clone(&bad)),
            ("http://good:1234", Arc::clone(&good)),
        ]);

        monitor(Arc::clone(&pool)).check_once().await;

        assert_eq!(pool.active_index(), 1, "must fail over to the healthy endpoint");
        assert_eq!(bad.probe_count(), 1, "active endpoint probed once");
        assert_eq!(good.probe_count(), 1, "fallback endpoint probed once");
    }

    #[tokio::test]
    async fn monitor_no_change_when_active_healthy() {
        let active = FakeProvider::healthy();
        let other = FakeProvider::healthy();
        let pool = pool_of(vec![
            ("http://a:1234", Arc::clone(&active)),
            ("http://b:1234", Arc::clone(&other)),
        ]);

        monitor(Arc::clone(&pool)).check_once().await;

        assert_eq!(pool.active_index(), 0, "no failover required");
        assert_eq!(active.probe_count(), 1);
        assert_eq!(other.probe_count(), 0, "other endpoints are not probed when active is healthy");
    }

    #[tokio::test]
    async fn monitor_keeps_current_when_all_unhealthy() {
        let a = FakeProvider::unhealthy();
        let b = FakeProvider::unhealthy();
        let pool =
            pool_of(vec![("http://a:1234", Arc::clone(&a)), ("http://b:1234", Arc::clone(&b))]);

        monitor(Arc::clone(&pool)).check_once().await;

        assert_eq!(pool.active_index(), 0, "no healthy alternative — keep current");
        assert_eq!(a.probe_count(), 1);
        assert_eq!(b.probe_count(), 1, "other candidates probed during search");
    }

    #[tokio::test]
    async fn monitor_recovers_after_active_heals() {
        let a = FakeProvider::unhealthy();
        let b = FakeProvider::healthy();
        let pool =
            pool_of(vec![("http://a:1234", Arc::clone(&a)), ("http://b:1234", Arc::clone(&b))]);

        let m = monitor(Arc::clone(&pool));
        m.check_once().await;
        assert_eq!(pool.active_index(), 1, "first failover lands on b");

        b.set_healthy(false);
        a.set_healthy(true);

        m.check_once().await;
        assert_eq!(pool.active_index(), 0, "monitor must fail back to recovered endpoint");
    }

    #[tokio::test]
    async fn monitor_run_returns_for_single_endpoint_pools() {
        let p = FakeProvider::healthy();
        let pool = pool_of(vec![("http://only:1234", Arc::clone(&p))]);
        let token = CancellationToken::new();

        monitor(Arc::clone(&pool)).run(token.clone()).await;
        assert_eq!(p.probe_count(), 0, "single-endpoint pool must not probe");
    }

    /// Exercises the production [`StallTracker::record`] state machine
    /// directly. The end-to-end wiring through alloy providers is covered
    /// by the integration tests in `tests/failover.rs`.
    #[test]
    fn stall_tracker_advance_resets_counter() {
        let mut t = StallTracker { last_block: 100, stalls: 5 };
        assert!(t.record(101, 2).is_ok());
        assert_eq!(t.last_block, 101);
        assert_eq!(t.stalls, 0, "advancing the head must reset the stall counter");
    }

    #[test]
    fn stall_tracker_within_tolerance_returns_ok() {
        // max_stalls=2: stall=1 is within tolerance, stall=2 trips the threshold.
        let mut t = StallTracker { last_block: 100, stalls: 0 };
        assert!(t.record(100, 2).is_ok(), "stall=1 within max_stalls=2");
        assert_eq!(t.stalls, 1);
    }

    #[test]
    fn stall_tracker_flags_when_counter_reaches_max() {
        let mut t = StallTracker { last_block: 100, stalls: 1 };
        let err = t.record(100, 2).expect_err("stall=2 must reach max_stalls=2");
        assert!(err.contains("not advancing"), "error must describe the stall: {err}");
        assert!(err.contains("100"), "error must include the stuck block: {err}");
    }

    #[test]
    fn stall_tracker_lower_block_is_treated_as_stall() {
        // A reorg or a passive replica that's behind reports a lower block
        // than last_seen — counts as a stall, not an advance.
        let mut t = StallTracker { last_block: 100, stalls: 0 };
        assert!(t.record(99, 2).is_ok(), "lower block counts as stall=1");
        assert_eq!(t.stalls, 1);
        assert_eq!(t.last_block, 100, "lower block must not regress last_seen");
    }

    /// First observation per endpoint primes state and must NOT count as a
    /// stall. Calls the production [`StallTracker::prime_or_record`] method
    /// (the same code path used by [`Probe::head_advancement`]) so a change
    /// in production logic is detected by this test.
    #[test]
    fn prime_or_record_first_tick_does_not_count_as_stall() {
        let mut state = HashMap::new();
        let max_stalls = 1u32; // any stall would trip — proves prime is special

        // First observation on a fresh endpoint: must Ok.
        assert!(
            StallTracker::prime_or_record(&mut state, 0, 100, max_stalls).is_ok(),
            "first tick primes without counting a stall"
        );

        // Second observation at the same block: stall=1 == max_stalls=1 → Err.
        assert!(
            StallTracker::prime_or_record(&mut state, 0, 100, max_stalls).is_err(),
            "subsequent stall at threshold flags"
        );
    }
}
