//! RAII guard for tracking in-flight operations.

/// RAII guard for tracking in-flight operations.
///
/// Increments a gauge on creation and decrements it on drop. Records the
/// outcome to a labeled counter on drop — defaulting to a caller-provided
/// value so that cancelled futures are always accounted for.
///
/// Use [`set_outcome`](Self::set_outcome) to override the default before drop.
///
/// Prefer the [`inflight!`] macro to construct this type.
#[cfg(feature = "metrics")]
pub struct InflightCounter {
    gauge: metrics::Gauge,
    counter_fn: fn(&'static str) -> metrics::Counter,
    outcome: &'static str,
}

/// No-op guard used when the `metrics` feature is disabled.
#[cfg(not(feature = "metrics"))]
pub struct InflightCounter;

#[cfg(feature = "metrics")]
impl core::fmt::Debug for InflightCounter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InflightCounter").finish_non_exhaustive()
    }
}

#[cfg(not(feature = "metrics"))]
impl core::fmt::Debug for InflightCounter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InflightCounter").finish()
    }
}

#[cfg(feature = "metrics")]
impl InflightCounter {
    /// Creates a new guard that increments the gauge immediately.
    #[inline]
    pub fn new(
        gauge: metrics::Gauge,
        counter_fn: fn(&'static str) -> metrics::Counter,
        outcome: &'static str,
    ) -> Self {
        gauge.increment(1);
        Self { gauge, counter_fn, outcome }
    }

    /// Overrides the outcome that will be recorded when this guard drops.
    #[inline]
    pub const fn set_outcome(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

#[cfg(not(feature = "metrics"))]
impl InflightCounter {
    /// Creates a no-op guard.
    #[inline]
    pub const fn new() -> Self {
        Self
    }

    /// No-op.
    #[inline]
    pub const fn set_outcome(&mut self, _outcome: &'static str) {}
}

#[cfg(feature = "metrics")]
impl Drop for InflightCounter {
    fn drop(&mut self) {
        self.gauge.decrement(1);
        (self.counter_fn)(self.outcome).increment(1);
    }
}

/// Creates an [`InflightCounter`] that tracks an in-flight operation.
///
/// # Examples
///
/// ```ignore
/// let mut guard = base_metrics::inflight!(
///     Metrics::in_flight_proofs(),
///     Metrics::requests_result_total,
///     Metrics::OUTCOME_DROPPED,
/// );
/// let result = do_work().await;
/// guard.set_outcome(Metrics::OUTCOME_SUCCESS);
/// // gauge decremented and outcome counter incremented on drop
/// ```
#[macro_export]
macro_rules! inflight {
    ($gauge:expr, $counter_fn:expr, $default_outcome:expr $(,)?) => {{
        #[cfg(feature = "metrics")]
        {
            $crate::InflightCounter::new($gauge, $counter_fn, $default_outcome)
        }
        #[cfg(not(feature = "metrics"))]
        {
            let _ = &$gauge;
            let _ = $default_outcome;
            $crate::InflightCounter::new()
        }
    }};
}
