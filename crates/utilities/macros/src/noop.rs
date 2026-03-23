//! No-op metric types used when the `metrics` feature is disabled.

/// A no-op metric handle used when the `metrics` feature is disabled.
///
/// Every method is `#[inline(always)]` and compiles to nothing, so call sites
/// that use the unified [`define_metrics!`] macro pay zero cost when metrics
/// are not enabled.
#[derive(Debug, Clone, Copy)]
pub struct NoopMetric;

impl NoopMetric {
    /// No-op `Gauge::set`.
    #[inline(always)]
    pub fn set<T>(&self, _: T) {}
    /// No-op `Counter::increment`.
    #[inline(always)]
    pub fn increment<T>(&self, _: T) {}
    /// No-op `Counter::absolute`.
    #[inline(always)]
    pub fn absolute<T>(&self, _: T) {}
    /// No-op `Histogram::record`.
    #[inline(always)]
    pub fn record<T>(&self, _: T) {}
    /// No-op `Gauge::decrement`.
    #[inline(always)]
    pub fn decrement<T>(&self, _: T) {}
}

/// No-op drop timer used when the `metrics` feature is disabled.
///
/// Zero-size, `no_std`-compatible. All methods compile to nothing.
#[derive(Debug, Clone, Copy)]
pub struct NoopDropTimer;

impl NoopDropTimer {
    /// No-op.
    #[inline(always)]
    pub const fn stop(&mut self) {}
}
