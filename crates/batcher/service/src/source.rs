//! RPC-based polling source for the latest unsafe L2 head block.

use std::sync::{Arc, Mutex};

use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockNumberOrTag;
use async_trait::async_trait;
use base_batcher_source::{PollingSource, SourceError};
use base_common_consensus::BaseBlock;
use base_common_network::Base;

use crate::EndpointPool;

/// Type alias for the L2 endpoint pool used by the polling source.
pub type L2EndpointPool = EndpointPool<dyn Provider<Base> + Send + Sync>;

/// Polling source that fetches the latest unsafe head block from an L2 RPC.
///
/// In normal operation (`new`) the source polls `Latest` on every call.
/// When created with `new_from`, it first catches up sequentially from the
/// given block number before switching to `Latest` polling. This avoids
/// missing blocks between `safe_head + 1` and `latest` on batcher restart.
///
/// The provider is resolved through an [`L2EndpointPool`] on every RPC call,
/// so failover decisions made by [`HealthMonitor`](crate::HealthMonitor) take
/// effect on the next call without requiring this source to be rebuilt.
#[derive(derive_more::Debug)]
pub struct RpcPollingSource {
    /// Pool of L2 RPC providers; the active one is resolved per call.
    #[debug(skip)]
    pool: Arc<L2EndpointPool>,
    /// When `Some(n)`, the next `unsafe_head` call fetches block `n`
    /// sequentially (for startup catchup). Cleared when `n` is strictly
    /// greater than the current latest (i.e. block `n` hasn't been produced yet).
    #[debug(skip)]
    next_sequential: Mutex<Option<u64>>,
}

impl RpcPollingSource {
    /// Create a new [`RpcPollingSource`] that polls `Latest` on every call.
    pub fn new(pool: Arc<L2EndpointPool>) -> Self {
        Self { pool, next_sequential: Mutex::new(None) }
    }

    /// Create a new [`RpcPollingSource`] that begins sequential catchup from
    /// `start_from`, fetching blocks `start_from, start_from+1, …` in order
    /// before switching to `Latest` polling once it has caught up.
    pub fn new_from(pool: Arc<L2EndpointPool>, start_from: u64) -> Self {
        Self { pool, next_sequential: Mutex::new(Some(start_from)) }
    }
}

#[async_trait]
impl PollingSource for RpcPollingSource {
    async fn unsafe_head(&self) -> Result<BaseBlock, SourceError> {
        match self.fetch_head().await {
            Ok(block) => {
                self.pool.record_call_success();
                Ok(block)
            }
            Err(e) => {
                // Circuit-breaker rotate: tolerate one transient error,
                // rotate on the second consecutive failure so we don't
                // drift away from the operator's priority order on every
                // network blip. The monitor owns the authoritative probe.
                self.pool.record_call_failure();
                Err(e)
            }
        }
    }

    fn reset_catchup(&self, start_from: u64) {
        *self.next_sequential.lock().unwrap() = Some(start_from);
    }

    fn is_catching_up(&self) -> bool {
        self.next_sequential.lock().unwrap().is_some()
    }
}

impl RpcPollingSource {
    async fn fetch_head(&self) -> Result<BaseBlock, SourceError> {
        let sequential = *self.next_sequential.lock().unwrap();

        if let Some(n) = sequential {
            let provider = self.pool.active();
            let latest_number = provider
                .get_block_number()
                .await
                .map_err(|e| SourceError::Provider(e.to_string()))?;

            if n > latest_number {
                // Block n hasn't been produced yet; switch to normal polling.
                *self.next_sequential.lock().unwrap() = None;
            } else {
                let block = provider
                    .get_block_by_number(n.into())
                    .full()
                    .await
                    .map_err(|e| SourceError::Provider(e.to_string()))?
                    .ok_or_else(|| SourceError::Provider(format!("block {n} not found")))?
                    .into_consensus()
                    .map_transactions(|t| t.inner.into_inner());
                *self.next_sequential.lock().unwrap() = Some(n + 1);
                return Ok(block);
            }
        }

        let provider = self.pool.active();
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .full()
            .await
            .map_err(|e| SourceError::Provider(e.to_string()))?
            .ok_or_else(|| SourceError::Provider("latest block not found".to_string()))?
            .into_consensus()
            .map_transactions(|t| t.inner.into_inner());
        Ok(block)
    }
}
