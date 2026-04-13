//! Prometheus metrics for snapshot operations.

#[cfg(feature = "metrics")]
base_metrics::define_metrics! {
    snapshot
    #[describe("Block height of the last successful snapshot")]
    snapshot_last_height: gauge,
    #[describe("Unix timestamp of the last successful snapshot")]
    snapshot_last_timestamp: gauge,
    #[describe("Duration of the last snapshot cycle in seconds")]
    snapshot_duration_seconds: histogram,
    #[describe("Node downtime during snapshot in seconds")]
    snapshot_downtime_seconds: histogram,
    #[describe("Total bytes uploaded to R2")]
    snapshot_upload_bytes_total: counter,
    #[describe("Duration of R2 upload in seconds")]
    snapshot_upload_duration_seconds: histogram,
    #[describe("Number of static file chunks skipped")]
    snapshot_chunks_skipped: counter,
    #[describe("Number of static file chunks uploaded")]
    snapshot_chunks_uploaded: counter,
    #[describe("Total snapshot errors")]
    snapshot_errors_total: counter,
}

/// No-op metrics when the `metrics` feature is disabled.
#[cfg(not(feature = "metrics"))]
#[derive(Debug, Clone, Default)]
pub struct Metrics;
