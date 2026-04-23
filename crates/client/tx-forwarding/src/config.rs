//! Configuration for the transaction forwarding extension.

use std::time::Duration;

use base_execution_txpool::{
    ConsumerConfig as TxpoolConsumerConfig, ForwarderConfig as TxpoolForwarderConfig,
};
use url::Url;

use crate::audit_config::{
    AuditDispatcherConfig, DEFAULT_AUDIT_BATCH_SIZE, DEFAULT_AUDIT_CHANNEL_SIZE,
    DEFAULT_AUDIT_MAX_RPS,
};

/// Default resend-after window in milliseconds (~2 blocks on Base).
pub const DEFAULT_RESEND_AFTER_MS: u64 = 4000;
/// Default maximum number of transactions per RPC batch.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;
/// Default maximum RPC requests per second per forwarder.
pub const DEFAULT_MAX_RPS: u32 = 200;
/// Full configuration for the transaction forwarding extension.
#[derive(Debug, Clone)]
pub struct TxForwardingConfig {
    /// Whether transaction forwarding is enabled.
    pub enabled: bool,
    /// Builder RPC endpoints to forward transactions to.
    pub builder_urls: Vec<Url>,
    /// Resend transactions that haven't been included after this duration in milliseconds.
    pub resend_after_ms: u64,
    /// Maximum number of transactions per batch (0 = unlimited).
    pub max_batch_size: usize,
    /// Maximum RPC requests per second per forwarder (0 = unlimited).
    pub max_rps: u32,
    /// URL of the audit service for forwarding events.
    pub audit_url: Option<Url>,
    /// Channel buffer size for audit events.
    pub audit_channel_size: usize,
    /// Max batch size for audit RPC.
    pub audit_batch_size: usize,
    /// Max RPS for audit RPC.
    pub audit_max_rps: u32,
}

impl Default for TxForwardingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            builder_urls: Vec::new(),
            resend_after_ms: DEFAULT_RESEND_AFTER_MS,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_rps: DEFAULT_MAX_RPS,
            audit_url: None,
            audit_channel_size: DEFAULT_AUDIT_CHANNEL_SIZE,
            audit_batch_size: DEFAULT_AUDIT_BATCH_SIZE,
            audit_max_rps: DEFAULT_AUDIT_MAX_RPS,
        }
    }
}

impl TxForwardingConfig {
    /// Creates a disabled configuration.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Creates a new configuration with forwarding enabled.
    pub fn new(builder_urls: Vec<Url>) -> Self {
        Self { enabled: true, builder_urls, ..Default::default() }
    }

    /// Sets the resend-after window in milliseconds.
    pub const fn with_resend_after_ms(mut self, ms: u64) -> Self {
        self.resend_after_ms = ms;
        self
    }

    /// Sets the maximum batch size per RPC request.
    pub const fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Sets the maximum RPC requests per second.
    pub const fn with_max_rps(mut self, rps: u32) -> Self {
        self.max_rps = rps;
        self
    }

    /// Converts to the consumer config used by `base-txpool`.
    pub fn to_consumer_config(&self) -> TxpoolConsumerConfig {
        TxpoolConsumerConfig::default()
            .with_resend_after(Duration::from_millis(self.resend_after_ms))
    }

    /// Converts to the forwarder config used by `base-txpool`.
    pub fn to_forwarder_config(&self) -> TxpoolForwarderConfig {
        TxpoolForwarderConfig::default()
            .with_builder_urls(self.builder_urls.clone())
            .with_max_batch_size(self.max_batch_size)
            .with_max_rps(self.max_rps)
    }

    /// Sets the audit service URL.
    pub fn with_audit_url(mut self, url: Option<Url>) -> Self {
        self.audit_url = url;
        self
    }

    /// Sets the audit channel size.
    pub const fn with_audit_channel_size(mut self, size: usize) -> Self {
        self.audit_channel_size = size;
        self
    }

    /// Sets the audit batch size.
    pub const fn with_audit_batch_size(mut self, size: usize) -> Self {
        self.audit_batch_size = size;
        self
    }

    /// Sets the audit max RPS.
    pub const fn with_audit_max_rps(mut self, rps: u32) -> Self {
        self.audit_max_rps = rps;
        self
    }

    /// Converts to audit dispatcher config if audit URL is set.
    pub fn to_audit_dispatcher_config(&self) -> Option<AuditDispatcherConfig> {
        self.audit_url.clone().map(|url| {
            AuditDispatcherConfig::new(url)
                .with_channel_size(self.audit_channel_size)
                .with_max_batch_size(self.audit_batch_size)
                .with_max_rps(self.audit_max_rps)
        })
    }
}
