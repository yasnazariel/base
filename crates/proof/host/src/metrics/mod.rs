//! Metrics for the proof host.
//!
//! All metric names are prefixed with `base_proof_host_`.
//!
//! ## Counters
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_requests_total` | `mode` | Total proof requests received |
//! | `base_proof_host_requests_result_total` | `outcome` | Proof request outcomes (incl. `dropped`) |
//! | `base_proof_host_hint_requests_total` | `hint_type` | Hint requests by type |
//! | `base_proof_host_hint_errors_total` | `hint_type` | Hint errors by type |
//! | `base_proof_host_kv_cold_lookups_total` | | KV lookups that missed the cache (resolved via hint fetch) |
//! | `base_proof_host_preimage_accesses_total` | | Total preimage accesses |
//! | `base_proof_host_offline_misses_total` | | Offline backend key misses |
//!
//! ## Gauges
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_in_flight_proofs` | | Currently in-flight proof requests |
//! | `base_proof_host_preimage_count` | | Preimage count from last witness build |
//!
//! ## Histograms
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_proof_duration_seconds` | | End-to-end proof generation duration |
//! | `base_proof_host_witness_build_duration_seconds` | | Witness build duration |
//! | `base_proof_host_prover_duration_seconds` | | Backend prover duration |
//! | `base_proof_host_hint_duration_seconds` | `hint_type` | Hint processing duration by type |
//! | `base_proof_host_replay_duration_seconds` | | Client replay (prologue+execute+validate) duration |

base_macros::define_metrics! {
    base_proof_host

    #[describe("Total proof requests received")]
    #[label("mode", mode)]
    requests_total: counter,

    #[describe("Proof request outcomes by result")]
    #[label("outcome", outcome)]
    requests_result_total: counter,

    #[describe("Hint requests by type")]
    #[label("hint_type", hint_type)]
    hint_requests_total: counter,

    #[describe("Hint processing errors by type")]
    #[label("hint_type", hint_type)]
    hint_errors_total: counter,

    #[describe("KV lookups that missed the cache and required hint fetching")]
    kv_cold_lookups_total: counter,

    #[describe("Total preimage accesses through the recording oracle")]
    preimage_accesses_total: counter,

    #[describe("Offline backend key-not-found events")]
    offline_misses_total: counter,

    #[describe("Currently in-flight proof requests")]
    in_flight_proofs: gauge,

    #[describe("Number of preimages captured in the last witness build")]
    preimage_count: gauge,

    #[describe("End-to-end proof generation duration")]
    proof_duration_seconds: histogram,

    #[describe("Witness build duration")]
    witness_build_duration_seconds: histogram,

    #[describe("Backend prover duration")]
    prover_duration_seconds: histogram,

    #[describe("Per-hint-type processing duration")]
    #[label("hint_type", hint_type)]
    hint_duration_seconds: histogram,

    #[describe("Client replay duration")]
    replay_duration_seconds: histogram,
}

impl Metrics {
    /// Online operating mode.
    pub const MODE_ONLINE: &str = "online";

    /// Successful proof outcome.
    pub const OUTCOME_SUCCESS: &str = "success";

    /// Witness generation error outcome.
    pub const OUTCOME_WITNESS_ERROR: &str = "witness_error";

    /// Backend proving error outcome.
    pub const OUTCOME_PROVE_ERROR: &str = "prove_error";

    /// Future was cancelled (dropped) before completion.
    pub const OUTCOME_DROPPED: &str = "dropped";
}

/// RAII timer that records elapsed duration to a histogram metric on drop.
///
/// Call [`.stop()`](Self::stop) to record early; otherwise the duration is
/// recorded when the guard is dropped.
#[cfg(feature = "metrics")]
pub struct DropTimer {
    histogram: metrics::Histogram,
    start: std::time::Instant,
    stopped: bool,
}

/// No-op timer used when the `metrics` feature is disabled.
#[cfg(not(feature = "metrics"))]
pub struct DropTimer;

#[cfg(feature = "metrics")]
impl std::fmt::Debug for DropTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DropTimer").finish_non_exhaustive()
    }
}

#[cfg(not(feature = "metrics"))]
impl std::fmt::Debug for DropTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DropTimer").finish()
    }
}

#[cfg(feature = "metrics")]
impl DropTimer {
    /// Creates a new timer. Use the [`timed!`] macro instead.
    #[inline]
    pub fn new(histogram: metrics::Histogram) -> Self {
        Self { histogram, start: std::time::Instant::now(), stopped: false }
    }

    /// Stops the timer, recording the elapsed duration to the histogram.
    ///
    /// Subsequent calls and the drop are no-ops.
    #[inline]
    pub fn stop(&mut self) {
        if !self.stopped {
            self.histogram.record(self.start.elapsed().as_secs_f64());
            self.stopped = true;
        }
    }
}

#[cfg(not(feature = "metrics"))]
impl DropTimer {
    /// Creates a no-op timer.
    #[inline]
    pub const fn new() -> Self {
        Self
    }

    /// No-op.
    #[inline]
    pub fn stop(&mut self) {}
}

#[cfg(feature = "metrics")]
impl Drop for DropTimer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// RAII guard for in-flight proof tracking.
///
/// Increments a gauge on creation and decrements it on drop. Records the
/// outcome to a counter on drop — defaulting to [`Metrics::OUTCOME_DROPPED`]
/// so that cancelled futures are always accounted for.
///
/// Use [`set_outcome`](Self::set_outcome) on the success/error path to
/// override the default before the guard drops.
#[cfg(feature = "metrics")]
pub struct ProofGuard {
    outcome: &'static str,
}

/// No-op guard used when the `metrics` feature is disabled.
#[cfg(not(feature = "metrics"))]
pub struct ProofGuard;

#[cfg(feature = "metrics")]
impl std::fmt::Debug for ProofGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofGuard").finish_non_exhaustive()
    }
}

#[cfg(not(feature = "metrics"))]
impl std::fmt::Debug for ProofGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofGuard").finish()
    }
}

#[cfg(feature = "metrics")]
impl Default for ProofGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "metrics")]
impl ProofGuard {
    /// Creates a new guard. Prefer the [`proof_guard!`] macro.
    #[inline]
    pub fn new() -> Self {
        Metrics::in_flight_proofs().increment(1);
        Self { outcome: Metrics::OUTCOME_DROPPED }
    }

    /// Overrides the outcome that will be recorded when this guard drops.
    #[inline]
    pub const fn set_outcome(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

#[cfg(not(feature = "metrics"))]
impl ProofGuard {
    /// Creates a no-op guard.
    #[inline]
    pub const fn new() -> Self {
        Self
    }

    /// No-op.
    #[inline]
    pub fn set_outcome(&mut self, _outcome: &'static str) {}
}

#[cfg(feature = "metrics")]
impl Drop for ProofGuard {
    fn drop(&mut self) {
        Metrics::in_flight_proofs().decrement(1);
        Metrics::requests_result_total(self.outcome).increment(1);
    }
}

/// Creates a [`DropTimer`] that records elapsed duration to a histogram.
///
/// # Examples
///
/// ```ignore
/// // Drop-based: records when `_timer` goes out of scope.
/// let _timer = timed!(Metrics::proof_duration_seconds());
///
/// // Explicit stop: records immediately, drop is a no-op.
/// let mut timer = timed!(Metrics::witness_build_duration_seconds());
/// let result = do_work().await;
/// timer.stop();
///
/// // With labels:
/// let _timer = timed!(Metrics::hint_duration_seconds(label));
/// ```
macro_rules! timed {
    ($metric_handle:expr) => {{
        #[cfg(feature = "metrics")]
        {
            $crate::DropTimer::new($metric_handle)
        }
        #[cfg(not(feature = "metrics"))]
        {
            let _ = &$metric_handle;
            $crate::DropTimer::new()
        }
    }};
}

pub(crate) use timed;

/// Creates a [`ProofGuard`] that tracks an in-flight proof.
///
/// # Examples
///
/// ```ignore
/// let mut guard = proof_guard!();
/// let result = do_work().await;
/// guard.set_outcome(Metrics::OUTCOME_SUCCESS);
/// // gauge decremented and outcome counter incremented on drop
/// ```
macro_rules! proof_guard {
    () => {{
        #[cfg(feature = "metrics")]
        {
            $crate::ProofGuard::new()
        }
        #[cfg(not(feature = "metrics"))]
        {
            $crate::ProofGuard::new()
        }
    }};
}

pub(crate) use proof_guard;
