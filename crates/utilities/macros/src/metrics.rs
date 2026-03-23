//! Macros for recording metrics.

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

/// Defines a metrics struct with static associated functions.
///
/// Each field becomes a function that returns the appropriate `metrics` handle
/// (or [`NoopMetric`] when the `metrics` feature is disabled).
///
/// # Attributes
///
/// - `#[scope("prefix")]` — required on the struct; prepended to every metric
///   name with an underscore separator.
/// - `#[describe("...")]` — optional per-field; registers a human-readable
///   description via `metrics::describe_*!`.
/// - `#[label("key", param)]` — optional per-field (may be repeated for
///   multi-label metrics); adds a labeled parameter to the generated function.
///   The first argument is the metric label key (string), the second is the
///   Rust parameter name (ident). Up to two labels are supported.
///
/// # Supported types
///
/// `counter`, `gauge`, `histogram`
///
/// # Example
///
/// ```ignore
/// // Struct name defaults to `Metrics` when omitted:
/// base_macros::define_metrics! {
///     #[scope("my_app")]
///     pub {
///         #[describe("Total requests")]
///         requests_total: counter,
///     }
/// }
/// Metrics::requests_total().increment(1);
///
/// // Custom struct name:
/// base_macros::define_metrics! {
///     #[scope("my_app")]
///     pub struct MyMetrics {
///         #[describe("Request duration")]
///         #[label("method", method)]
///         request_duration: histogram,
///
///         #[label("kind", kind)]
///         #[label("reason", reason)]
///         errors: counter,
///     }
/// }
/// MyMetrics::request_duration("GET").record(0.42);
/// MyMetrics::errors("dial", "timeout").increment(1);
/// ```
#[macro_export]
macro_rules! define_metrics {
    (
        #[scope($scope:expr)]
        pub {
            $(
                $(#[describe($desc:expr)])?
                $(#[label($label_key:expr, $label_param:ident)])*
                $field:ident : $kind:ident
            ),*
            $(,)?
        }
    ) => {
        $crate::define_metrics! {
            #[scope($scope)]
            pub struct Metrics {
                $(
                    $(#[describe($desc)])?
                    $(#[label($label_key, $label_param)])*
                    $field : $kind
                ),*
            }
        }
    };
    (
        #[scope($scope:expr)]
        $vis:vis struct $name:ident {
            $(
                $(#[describe($desc:expr)])?
                $(#[label($label_key:expr, $label_param:ident)])*
                $field:ident : $kind:ident
            ),*
            $(,)?
        }
    ) => {
        #[allow(missing_docs)]
        $vis struct $name;

        #[allow(missing_docs)]
        impl $name {
            $(
                $crate::__define_metric_fn!(
                    $scope, $field, $kind
                    $(; describe = $desc)?
                    $(; label_key = $label_key, label_param = $label_param)*
                );
            )*

            /// Registers human-readable descriptions for all metrics.
            #[cfg(feature = "metrics")]
            pub fn describe() {
                $(
                    $crate::__describe_metric!(
                        $scope, $field, $kind
                        $(, $desc)?
                    );
                )*
            }

            /// No-op when the `metrics` feature is disabled.
            #[cfg(not(feature = "metrics"))]
            #[inline(always)]
            pub fn describe() {}
        }
    };
}

/// Internal helper — generates a single metric accessor function.
#[doc(hidden)]
#[macro_export]
macro_rules! __define_metric_fn {
    // ── Two-label counter ──
    ($scope:expr, $field:ident, counter; describe = $desc:expr; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 counter Counter $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    ($scope:expr, $field:ident, counter; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 counter Counter $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    // ── Two-label gauge ──
    ($scope:expr, $field:ident, gauge; describe = $desc:expr; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 gauge Gauge $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    ($scope:expr, $field:ident, gauge; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 gauge Gauge $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    // ── Two-label histogram ──
    ($scope:expr, $field:ident, histogram; describe = $desc:expr; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 histogram Histogram $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    ($scope:expr, $field:ident, histogram; label_key = $lk1:expr, label_param = $lp1:ident; label_key = $lk2:expr, label_param = $lp2:ident) => {
        $crate::__define_metric_fn!(@labeled2 histogram Histogram $scope, $field, $lk1, $lp1, $lk2, $lp2);
    };
    // ── Labeled counter ──
    ($scope:expr, $field:ident, counter; describe = $desc:expr; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled counter Counter $scope, $field, $lk, $lp);
    };
    ($scope:expr, $field:ident, counter; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled counter Counter $scope, $field, $lk, $lp);
    };
    // ── Labeled gauge ──
    ($scope:expr, $field:ident, gauge; describe = $desc:expr; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled gauge Gauge $scope, $field, $lk, $lp);
    };
    ($scope:expr, $field:ident, gauge; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled gauge Gauge $scope, $field, $lk, $lp);
    };
    // ── Labeled histogram ──
    ($scope:expr, $field:ident, histogram; describe = $desc:expr; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled histogram Histogram $scope, $field, $lk, $lp);
    };
    ($scope:expr, $field:ident, histogram; label_key = $lk:expr, label_param = $lp:ident) => {
        $crate::__define_metric_fn!(@labeled histogram Histogram $scope, $field, $lk, $lp);
    };
    // ── Unlabeled counter ──
    ($scope:expr, $field:ident, counter; describe = $desc:expr) => {
        $crate::__define_metric_fn!(@unlabeled counter Counter $scope, $field);
    };
    ($scope:expr, $field:ident, counter) => {
        $crate::__define_metric_fn!(@unlabeled counter Counter $scope, $field);
    };
    // ── Unlabeled gauge ──
    ($scope:expr, $field:ident, gauge; describe = $desc:expr) => {
        $crate::__define_metric_fn!(@unlabeled gauge Gauge $scope, $field);
    };
    ($scope:expr, $field:ident, gauge) => {
        $crate::__define_metric_fn!(@unlabeled gauge Gauge $scope, $field);
    };
    // ── Unlabeled histogram ──
    ($scope:expr, $field:ident, histogram; describe = $desc:expr) => {
        $crate::__define_metric_fn!(@unlabeled histogram Histogram $scope, $field);
    };
    ($scope:expr, $field:ident, histogram) => {
        $crate::__define_metric_fn!(@unlabeled histogram Histogram $scope, $field);
    };
    // ── Internal: two-label impl ──
    (@labeled2 $macro_name:ident $ret:ident $scope:expr, $field:ident, $lk1:expr, $lp1:ident, $lk2:expr, $lp2:ident) => {
        #[cfg(feature = "metrics")]
        #[allow(unused)]
        pub fn $field($lp1: &str, $lp2: &str) -> ::metrics::$ret {
            let v1 = $lp1.to_string();
            let v2 = $lp2.to_string();
            ::metrics::$macro_name!(concat!($scope, "_", stringify!($field)), $lk1 => v1, $lk2 => v2)
        }

        #[cfg(not(feature = "metrics"))]
        #[inline(always)]
        #[allow(unused)]
        pub fn $field(_: &str, _: &str) -> $crate::NoopMetric {
            $crate::NoopMetric
        }
    };
    // ── Internal: labeled impl ──
    (@labeled $macro_name:ident $ret:ident $scope:expr, $field:ident, $lk:expr, $lp:ident) => {
        #[cfg(feature = "metrics")]
        #[allow(unused)]
        pub fn $field($lp: &str) -> ::metrics::$ret {
            let label_value = $lp.to_string();
            ::metrics::$macro_name!(concat!($scope, "_", stringify!($field)), $lk => label_value)
        }

        #[cfg(not(feature = "metrics"))]
        #[inline(always)]
        #[allow(unused)]
        pub fn $field(_: &str) -> $crate::NoopMetric {
            $crate::NoopMetric
        }
    };
    // ── Internal: unlabeled impl ──
    (@unlabeled $macro_name:ident $ret:ident $scope:expr, $field:ident) => {
        #[cfg(feature = "metrics")]
        #[allow(unused)]
        pub fn $field() -> ::metrics::$ret {
            ::metrics::$macro_name!(concat!($scope, "_", stringify!($field)))
        }

        #[cfg(not(feature = "metrics"))]
        #[inline(always)]
        #[allow(unused)]
        pub fn $field() -> $crate::NoopMetric {
            $crate::NoopMetric
        }
    };
}

/// Internal helper — emits a `metrics::describe_*!` call when a description is provided.
#[doc(hidden)]
#[macro_export]
macro_rules! __describe_metric {
    ($scope:expr, $field:ident, counter, $desc:expr) => {
        ::metrics::describe_counter!(concat!($scope, "_", stringify!($field)), $desc);
    };
    ($scope:expr, $field:ident, gauge, $desc:expr) => {
        ::metrics::describe_gauge!(concat!($scope, "_", stringify!($field)), $desc);
    };
    ($scope:expr, $field:ident, histogram, $desc:expr) => {
        ::metrics::describe_histogram!(concat!($scope, "_", stringify!($field)), $desc);
    };
    // No description — skip.
    ($scope:expr, $field:ident, $kind:ident) => {};
}
