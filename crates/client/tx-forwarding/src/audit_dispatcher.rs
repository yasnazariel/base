//! Audit event dispatcher that batches and sends events to the audit service.

use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::B256;
use audit_archiver_lib::BundleEvent;
use jsonrpsee::{core::client::ClientT, http_client::HttpClient};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};

use crate::audit_config::AuditDispatcherConfig;

struct RateLimiter {
    timestamps: VecDeque<Instant>,
    max_rps: u32,
}

impl RateLimiter {
    fn new(max_rps: u32) -> Self {
        let capacity = if max_rps == 0 { 0 } else { max_rps as usize };
        Self { timestamps: VecDeque::with_capacity(capacity), max_rps }
    }

    fn prune(&mut self, now: Instant) {
        let window = Duration::from_secs(1);
        while let Some(&front) = self.timestamps.front() {
            if now.duration_since(front) >= window {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    fn check_rate_limit(&mut self) -> Option<Duration> {
        if self.max_rps == 0 {
            return None;
        }
        let now = Instant::now();
        self.prune(now);
        if (self.timestamps.len() as u32) < self.max_rps {
            return None;
        }
        let oldest = self.timestamps.front().expect("non-empty after prune");
        let window = Duration::from_secs(1);
        Some(window.saturating_sub(now.duration_since(*oldest)))
    }

    fn record_send(&mut self) {
        if self.max_rps > 0 {
            self.timestamps.push_back(Instant::now());
        }
    }
}

/// Audit event dispatcher that batches events and sends them to the audit service.
pub struct AuditDispatcher {
    client: HttpClient,
    receiver: mpsc::Receiver<BundleEvent>,
    config: Arc<AuditDispatcherConfig>,
    cancel: CancellationToken,
    limiter: RateLimiter,
    buffer: Vec<BundleEvent>,
    seen: HashSet<B256>,
}

impl AuditDispatcher {
    /// Creates a new audit dispatcher.
    pub fn new(
        client: HttpClient,
        receiver: mpsc::Receiver<BundleEvent>,
        config: Arc<AuditDispatcherConfig>,
        cancel: CancellationToken,
    ) -> Self {
        let limiter = RateLimiter::new(config.max_rps);
        let buffer = Vec::with_capacity(config.max_batch_size);
        let seen = HashSet::new();
        Self { client, receiver, config, cancel, limiter, buffer, seen }
    }

    /// Runs the dispatcher loop.
    pub async fn run(mut self) {
        info!(
            audit_url = %self.config.audit_url,
            max_rps = self.config.max_rps,
            max_batch_size = self.config.max_batch_size,
            "starting audit dispatcher"
        );

        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            match self.limiter.check_rate_limit() {
                None if !self.buffer.is_empty() => {
                    self.flush_buffer().await;
                    continue;
                }
                Some(wait) => {
                    tokio::select! {
                        _ = self.cancel.cancelled() => break,
                        _ = tokio::time::sleep(wait) => continue,
                        result = self.receiver.recv() => {
                            if self.handle_recv(result) {
                                break;
                            }
                        }
                    }
                    continue;
                }
                _ => {}
            }

            tokio::select! {
                _ = self.cancel.cancelled() => break,
                result = self.receiver.recv() => {
                    if self.handle_recv(result) {
                        break;
                    }
                }
            }

            if !self.buffer.is_empty() && self.limiter.check_rate_limit().is_none() {
                self.flush_buffer().await;
            }
        }

        self.flush_remaining().await;
    }

    fn handle_recv(&mut self, result: Option<BundleEvent>) -> bool {
        match result {
            Some(event) => {
                let tx_hash = Self::extract_tx_hash(&event);
                if self.seen.contains(&tx_hash) {
                    trace!(tx_hash = %tx_hash, "duplicate event, skipping");
                    return false;
                }
                self.seen.insert(tx_hash);
                self.buffer.push(event);
                false
            }
            None => {
                info!("audit channel closed");
                true
            }
        }
    }

    const fn extract_tx_hash(event: &BundleEvent) -> B256 {
        match event {
            BundleEvent::MempoolForwarded { tx_hash } | BundleEvent::MempoolDropped { tx_hash } => {
                *tx_hash
            }
            _ => B256::ZERO,
        }
    }

    async fn flush_remaining(&mut self) {
        while !self.buffer.is_empty() {
            self.flush_buffer().await;
        }
    }

    async fn flush_buffer(&mut self) {
        let batch_size = if self.config.max_batch_size == 0 {
            self.buffer.len()
        } else {
            self.buffer.len().min(self.config.max_batch_size)
        };
        let batch: Vec<BundleEvent> = self.buffer.drain(..batch_size).collect();

        if batch.is_empty() {
            return;
        }

        trace!(events = batch.len(), remaining = self.buffer.len(), "flushing audit batch");

        self.send_with_retries(batch).await;
        self.limiter.record_send();
    }

    async fn send_with_retries(&self, batch: Vec<BundleEvent>) {
        let event_count = batch.len();
        let overall_start = Instant::now();

        for attempt in 0..=self.config.max_retries {
            let result: Result<u32, _> =
                self.client.request("base_persistEvent", (batch.clone(),)).await;

            match result {
                Ok(persisted) => {
                    debug!(
                        events = event_count,
                        persisted = persisted,
                        duration_ms = overall_start.elapsed().as_millis() as u64,
                        "audit batch sent"
                    );
                    return;
                }
                Err(err) if Self::is_retryable(&err) && attempt < self.config.max_retries => {
                    let backoff = self.config.retry_backoff * 2u32.saturating_pow(attempt);
                    debug!(
                        attempt = attempt + 1,
                        max_retries = self.config.max_retries,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %err,
                        "audit RPC failed, retrying"
                    );
                    tokio::select! {
                        _ = self.cancel.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }
                Err(err) => {
                    error!(
                        error = %err,
                        events = event_count,
                        "audit RPC failed, dropping batch"
                    );
                    return;
                }
            }
        }
    }

    const fn is_retryable(err: &jsonrpsee::core::ClientError) -> bool {
        matches!(
            err,
            jsonrpsee::core::ClientError::Transport(_)
                | jsonrpsee::core::ClientError::RequestTimeout
                | jsonrpsee::core::ClientError::RestartNeeded(_)
        )
    }
}

impl std::fmt::Debug for AuditDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditDispatcher")
            .field("config", &self.config)
            .field("buffer_len", &self.buffer.len())
            .field("seen_count", &self.seen.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_unlimited_when_zero() {
        let mut limiter = RateLimiter::new(0);
        for _ in 0..10_000 {
            assert!(limiter.check_rate_limit().is_none());
            limiter.record_send();
        }
        assert!(limiter.timestamps.is_empty());
    }

    #[test]
    fn rate_limiter_enforces_limit() {
        let mut limiter = RateLimiter::new(3);
        for _ in 0..3 {
            assert!(limiter.check_rate_limit().is_none());
            limiter.record_send();
        }
        assert!(limiter.check_rate_limit().is_some());
    }

    #[test]
    fn rate_limiter_window_expires() {
        let mut limiter = RateLimiter::new(2);
        limiter.record_send();
        limiter.record_send();
        assert!(limiter.check_rate_limit().is_some());
        assert_eq!(limiter.timestamps.len(), 2);
    }

    #[test]
    fn dedup_by_tx_hash() {
        let mut seen = HashSet::new();
        let tx_hash = B256::from([1u8; 32]);

        assert!(!seen.contains(&tx_hash));
        seen.insert(tx_hash);
        assert!(seen.contains(&tx_hash));

        let tx_hash2 = B256::from([2u8; 32]);
        assert!(!seen.contains(&tx_hash2));
    }

    #[test]
    fn extract_tx_hash_from_mempool_forwarded() {
        let tx_hash = B256::from([5u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        assert_eq!(AuditDispatcher::extract_tx_hash(&event), tx_hash);
    }

    #[test]
    fn extract_tx_hash_from_mempool_dropped() {
        let tx_hash = B256::from([6u8; 32]);
        let event = BundleEvent::MempoolDropped { tx_hash };
        assert_eq!(AuditDispatcher::extract_tx_hash(&event), tx_hash);
    }
}
