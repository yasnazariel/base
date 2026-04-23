//! Configuration for the audit event dispatcher.

use std::time::Duration;

use url::Url;

/// Default channel buffer size.
pub const DEFAULT_AUDIT_CHANNEL_SIZE: usize = 10_000;
/// Default max batch size for audit RPC.
pub const DEFAULT_AUDIT_BATCH_SIZE: usize = 500;
/// Default max RPS for audit RPC.
pub const DEFAULT_AUDIT_MAX_RPS: u32 = 50;

/// Configuration for the audit event dispatcher.
#[derive(Debug, Clone)]
pub struct AuditDispatcherConfig {
    /// URL of the audit service RPC endpoint.
    pub audit_url: Url,
    /// Maximum events to buffer before backpressure.
    pub channel_size: usize,
    /// Maximum events per RPC batch.
    pub max_batch_size: usize,
    /// Maximum RPC requests per second.
    pub max_rps: u32,
    /// Maximum retries for RPC failures.
    pub max_retries: u32,
    /// Base delay between retries.
    pub retry_backoff: Duration,
    /// Per-request timeout.
    pub request_timeout: Duration,
}

impl Default for AuditDispatcherConfig {
    fn default() -> Self {
        Self {
            audit_url: "http://localhost:8546".parse().expect("valid default URL"),
            channel_size: DEFAULT_AUDIT_CHANNEL_SIZE,
            max_batch_size: DEFAULT_AUDIT_BATCH_SIZE,
            max_rps: DEFAULT_AUDIT_MAX_RPS,
            max_retries: 3,
            retry_backoff: Duration::from_millis(100),
            request_timeout: Duration::from_secs(2),
        }
    }
}

impl AuditDispatcherConfig {
    /// Creates a new config with the given audit URL.
    pub fn new(audit_url: Url) -> Self {
        Self { audit_url, ..Default::default() }
    }

    /// Sets the channel buffer size.
    pub const fn with_channel_size(mut self, size: usize) -> Self {
        self.channel_size = size;
        self
    }

    /// Sets the maximum batch size.
    pub const fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Sets the maximum requests per second.
    pub const fn with_max_rps(mut self, rps: u32) -> Self {
        self.max_rps = rps;
        self
    }

    /// Sets the maximum retries.
    pub const fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Sets the retry backoff duration.
    pub const fn with_retry_backoff(mut self, backoff: Duration) -> Self {
        self.retry_backoff = backoff;
        self
    }

    /// Sets the request timeout duration.
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
}
