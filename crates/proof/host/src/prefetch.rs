//! L1 header prefetching and rate-limited RPC access.
//!
//! When the proof program walks backwards through L1 headers (following `parent_hash`
//! pointers), each header triggers a separate `debug_getRawHeader` RPC call. This
//! module introduces [`L1HeaderPrefetcher`] which:
//!
//! - **Rate-limits** outbound L1 RPCs through a shared [`Semaphore`].
//! - **Retries** transient failures (transport errors, rate-limit responses) with
//!   exponential backoff.
//! - **Speculatively prefetches** parent headers in parallel by block number, so
//!   subsequent oracle requests hit the KV store instead of the RPC.

use std::{future::Future, sync::Arc, time::Duration};

use alloy_consensus::Header;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{B256, Bytes, keccak256};
use alloy_provider::{Provider, RootProvider};
use alloy_rlp::Decodable;
use alloy_rpc_types::Block;
use alloy_transport::TransportError;
use backon::{ExponentialBuilder, Retryable};
use base_proof_preimage::PreimageKey;
use dashmap::DashSet;
use tokio::{sync::Semaphore, task::JoinSet};
use tracing::{debug, trace};

use crate::{HostError, Metrics, Result, SharedKeyValueStore};

/// Default maximum concurrent L1 RPC requests.
pub const DEFAULT_L1_CONCURRENCY: usize = 8;

/// Default number of parent headers to speculatively prefetch.
pub const DEFAULT_PREFETCH_DEPTH: usize = 64;

const MAX_RETRY_ATTEMPTS: usize = 5;
const RETRY_MIN_DELAY: Duration = Duration::from_millis(100);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(10);

/// Manages rate-limited, retried L1 RPC calls and background header prefetching.
///
/// All L1 RPC requests flow through a shared [`Semaphore`] that caps the number
/// of in-flight calls. Transient errors (including rate-limit responses) are
/// automatically retried with exponential back-off. The semaphore permit is
/// acquired inside the retry closure so it is released during back-off sleep,
/// freeing the slot for other requests while this one waits to retry.
///
/// When an `L1BlockHeader` hint arrives, the prefetcher speculatively fetches
/// parent headers in parallel (bounded by the same semaphore) so subsequent
/// oracle look-ups find the data in the KV store and skip the RPC entirely.
pub struct L1HeaderPrefetcher {
    provider: RootProvider,
    semaphore: Arc<Semaphore>,
    kv: SharedKeyValueStore,
    prefetch_depth: usize,
    in_flight: Arc<DashSet<u64>>,
}

impl std::fmt::Debug for L1HeaderPrefetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("L1HeaderPrefetcher")
            .field("prefetch_depth", &self.prefetch_depth)
            .finish_non_exhaustive()
    }
}

impl L1HeaderPrefetcher {
    /// Creates a new [`L1HeaderPrefetcher`].
    ///
    /// # Panics
    /// Panics if `concurrency == 0`. Callers must enforce this at the boundary
    /// (e.g. via clap's `value_parser!(usize).range(1..)`).
    pub fn new(
        provider: RootProvider,
        kv: SharedKeyValueStore,
        concurrency: usize,
        prefetch_depth: usize,
    ) -> Self {
        assert!(concurrency > 0, "concurrency must be >= 1");

        Self {
            provider,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            kv,
            prefetch_depth,
            in_flight: Arc::new(DashSet::new()),
        }
    }

    /// Fetches an L1 header by hash and stores it in the KV store.
    ///
    /// Returns the decoded header so the caller can issue parent-prefetch
    /// without a second decode. If already in the KV store the RPC is skipped.
    pub async fn fetch_and_store_header(&self, hash: B256) -> Result<Header> {
        let key = PreimageKey::new_keccak256(*hash);

        {
            let kv = self.kv.read().await;
            if let Some(raw) = kv.get(key.into()) {
                return Header::decode(&mut raw.as_slice()).map_err(HostError::Rlp);
            }
        }

        let raw = self.fetch_raw_header_by_hash(hash).await?;
        let header = Header::decode(&mut raw.as_ref()).map_err(HostError::Rlp)?;
        self.kv.write().await.set(key.into(), raw.into())?;

        Ok(header)
    }

    /// Spawns a fire-and-forget background task that prefetches up to
    /// `prefetch_depth` parent headers below `header.number`.
    pub fn prefetch_parents(self: &Arc<Self>, header: &Header) {
        if self.prefetch_depth == 0 || header.number <= 1 {
            return;
        }

        let start = header.number - 1;
        // depth >= 1 here, so subtraction is safe; saturating_sub bounds at block 1.
        let end = start.saturating_sub((self.prefetch_depth - 1) as u64).max(1);

        let blocks: Vec<u64> = (end..=start).filter(|n| self.in_flight.insert(*n)).collect();

        if blocks.is_empty() {
            trace!(from_block = start, to_block = end, "all blocks already in-flight, skipping");
            return;
        }

        debug!(
            from_block = start,
            to_block = end,
            new_blocks = blocks.len(),
            "spawning L1 header prefetch"
        );

        let me = Arc::clone(self);
        tokio::spawn(async move { me.prefetch_range(blocks).await });
    }

    /// Fetches a full block by hash through the semaphore + retry layer.
    pub async fn fetch_block_by_hash(&self, hash: B256) -> Result<Option<Block>> {
        let provider = self.provider.clone();
        Ok(self
            .rpc("eth_getBlockByHash", move || {
                let provider = provider.clone();
                async move { provider.get_block_by_hash(hash).full().await }
            })
            .await?)
    }

    /// Fetches raw receipts by block hash through the semaphore + retry layer.
    pub async fn fetch_raw_receipts(&self, hash: B256) -> Result<Vec<Bytes>> {
        let provider = self.provider.clone();
        Ok(self
            .rpc("debug_getRawReceipts", move || {
                let provider = provider.clone();
                async move {
                    provider
                        .client()
                        .request::<[B256; 1], Vec<Bytes>>("debug_getRawReceipts", [hash])
                        .await
                }
            })
            .await?)
    }

    async fn fetch_raw_header_by_hash(
        &self,
        hash: B256,
    ) -> std::result::Result<Bytes, TransportError> {
        let provider = self.provider.clone();
        self.rpc("debug_getRawHeader[hash]", move || {
            let provider = provider.clone();
            async move {
                provider.client().request::<[B256; 1], Bytes>("debug_getRawHeader", [hash]).await
            }
        })
        .await
    }

    async fn fetch_raw_header_by_number(
        &self,
        block_number: u64,
    ) -> std::result::Result<Bytes, TransportError> {
        let provider = self.provider.clone();
        self.rpc("debug_getRawHeader[number]", move || {
            let provider = provider.clone();
            async move {
                provider
                    .client()
                    .request::<[BlockNumberOrTag; 1], Bytes>(
                        "debug_getRawHeader",
                        [BlockNumberOrTag::Number(block_number)],
                    )
                    .await
            }
        })
        .await
    }

    /// Runs `op` under the shared semaphore with exponential-backoff retry on
    /// transient transport errors. The semaphore permit is acquired inside the
    /// retry closure so it is released during back-off sleep.
    async fn rpc<T, F, Fut>(
        &self,
        op_name: &'static str,
        op: F,
    ) -> std::result::Result<T, TransportError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = std::result::Result<T, TransportError>>,
    {
        (|| async {
            let _permit =
                self.semaphore.acquire().await.expect("semaphore is owned and never closed");
            op().await
        })
        .retry(backoff_builder())
        .when(is_retryable_transport)
        .notify(|err, dur| {
            debug!(error = %err, delay = ?dur, op = op_name, "retrying L1 RPC");
        })
        .await
    }

    async fn prefetch_range(self: Arc<Self>, blocks: Vec<u64>) {
        let mut tasks = JoinSet::new();

        for block_number in blocks {
            let me = Arc::clone(&self);
            tasks.spawn(async move {
                let raw = match me.fetch_raw_header_by_number(block_number).await {
                    Ok(raw) => Some(raw),
                    Err(e) => {
                        debug!(block_number, error = %e, "L1 header prefetch failed");
                        None
                    }
                };
                me.in_flight.remove(&block_number);
                raw
            });
        }

        let entries: Vec<(PreimageKey, Bytes)> = tasks
            .join_all()
            .await
            .into_iter()
            .flatten()
            .map(|raw| (PreimageKey::new_keccak256(*keccak256(&raw)), raw))
            .collect();
        if entries.is_empty() {
            return;
        }

        let mut kv = self.kv.write().await;
        let mut stored: u64 = 0;
        for (key, raw) in entries {
            if let Err(e) = kv.set(key.into(), raw.into()) {
                debug!(error = %e, "L1 prefetch store failed");
            } else {
                stored += 1;
            }
        }
        if stored > 0 {
            Metrics::l1_prefetch_stored_total().increment(stored);
        }
    }
}

fn backoff_builder() -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(RETRY_MIN_DELAY)
        .with_max_delay(RETRY_MAX_DELAY)
        .with_max_times(MAX_RETRY_ATTEMPTS)
        .with_jitter()
}

fn is_retryable_transport(err: &TransportError) -> bool {
    matches!(err, TransportError::Transport(_)) || is_rate_limited(err)
}

/// Different RPC providers signal rate-limiting in different ways:
/// - HTTP 429 surfaced as a transport error string.
/// - JSON-RPC error code `429` (non-standard but common).
/// - JSON-RPC error code `-32005` (Infura/Alchemy rate limit).
/// - Message containing "rate limit" or "too many requests".
fn is_rate_limited(err: &TransportError) -> bool {
    match err {
        TransportError::ErrorResp(payload) => {
            if payload.code == 429 || payload.code == -32005 {
                return true;
            }
            let msg = payload.message.to_lowercase();
            msg.contains("rate limit") || msg.contains("too many requests")
        }
        TransportError::Transport(err) => {
            let msg = err.to_string().to_lowercase();
            msg.contains("429") || msg.contains("rate limit")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use alloy_json_rpc::ErrorPayload;

    use super::*;

    #[test]
    fn test_is_rate_limited_error_code_429() {
        let err = TransportError::ErrorResp(ErrorPayload {
            code: 429,
            message: "Too Many Requests".into(),
            data: None,
        });
        assert!(is_rate_limited(&err));
        assert!(is_retryable_transport(&err));
    }

    #[test]
    fn test_is_rate_limited_error_code_minus_32005() {
        let err = TransportError::ErrorResp(ErrorPayload {
            code: -32005,
            message: "daily request count exceeded".into(),
            data: None,
        });
        assert!(is_rate_limited(&err));
        assert!(is_retryable_transport(&err));
    }

    #[test]
    fn test_is_rate_limited_message_contains_rate_limit() {
        let err = TransportError::ErrorResp(ErrorPayload {
            code: -32000,
            message: "Rate Limit exceeded, try again later".into(),
            data: None,
        });
        assert!(is_rate_limited(&err));
    }

    #[test]
    fn test_non_retryable_error_resp() {
        let err = TransportError::ErrorResp(ErrorPayload {
            code: -32600,
            message: "invalid request".into(),
            data: None,
        });
        assert!(!is_rate_limited(&err));
        assert!(!is_retryable_transport(&err));
    }

    #[test]
    fn test_transport_error_is_retryable() {
        let err = alloy_transport::TransportErrorKind::custom_str("connection reset");
        assert!(is_retryable_transport(&err));
    }

    #[test]
    fn test_ser_error_is_not_retryable() {
        let err = TransportError::SerError(serde_json::Error::io(std::io::Error::other("test")));
        assert!(!is_retryable_transport(&err));
    }
}
