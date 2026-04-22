//! L1 head source implementations for the batcher service.

use std::sync::Arc;

use alloy_provider::Provider;
use async_trait::async_trait;
use base_batcher_source::{L1HeadPolling, L1HeadSubscription, SourceError};
use futures::{StreamExt, stream::BoxStream};

use crate::EndpointPool;

/// Type alias for the L1 endpoint pool used by the head poller.
pub type L1EndpointPool = EndpointPool<dyn Provider + Send + Sync>;

/// Polling source that fetches the latest L1 head block number from an L1 RPC endpoint.
///
/// Resolves the active provider through an [`L1EndpointPool`] on every call so
/// that runtime failover decisions made by
/// [`HealthMonitor`](crate::HealthMonitor) take effect immediately.
#[derive(derive_more::Debug)]
pub struct RpcL1HeadPollingSource {
    #[debug(skip)]
    pool: Arc<L1EndpointPool>,
}

impl RpcL1HeadPollingSource {
    /// Create a new [`RpcL1HeadPollingSource`] backed by the given endpoint pool.
    pub fn new(pool: Arc<L1EndpointPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl L1HeadPolling for RpcL1HeadPollingSource {
    async fn latest_head(&self) -> Result<u64, SourceError> {
        match self.pool.active().get_block_number().await {
            Ok(n) => {
                self.pool.record_call_success();
                Ok(n)
            }
            Err(e) => {
                self.pool.record_call_failure();
                Err(SourceError::Provider(e.to_string()))
            }
        }
    }
}

/// An [`L1HeadSubscription`] backed by a WebSocket provider.
///
/// Owns the WS provider via a type-erased [`Arc`] so the underlying connection
/// is not dropped when the stream is handed to [`HybridL1HeadSource`]. The stream
/// is produced once at construction; [`take_stream`] moves it out on the first call.
///
/// [`HybridL1HeadSource`]: base_batcher_source::HybridL1HeadSource
/// [`take_stream`]: L1HeadSubscription::take_stream
#[derive(derive_more::Debug)]
pub struct WsL1HeadSubscription {
    #[debug(skip)]
    _provider: Arc<dyn std::any::Any + Send + Sync>,
    #[debug("{:?}", stream.as_ref().map(|_| "<stream>"))]
    stream: Option<BoxStream<'static, Result<u64, SourceError>>>,
}

impl WsL1HeadSubscription {
    /// Create a new [`WsL1HeadSubscription`] from a provider and its head number stream.
    pub fn new<P: std::any::Any + Send + Sync + 'static>(
        provider: Arc<P>,
        stream: BoxStream<'static, Result<u64, SourceError>>,
    ) -> Self {
        Self { _provider: provider, stream: Some(stream) }
    }
}

impl L1HeadSubscription for WsL1HeadSubscription {
    fn take_stream(&mut self) -> BoxStream<'static, Result<u64, SourceError>> {
        self.stream.take().expect("take_stream called more than once")
    }
}

/// A no-op [`L1HeadSubscription`] that never yields head numbers.
///
/// Used when no L1 WebSocket URL is configured; [`HybridL1HeadSource`] falls
/// back entirely to the polling path.
///
/// [`HybridL1HeadSource`]: base_batcher_source::HybridL1HeadSource
#[derive(Debug)]
pub struct NullL1HeadSubscription;

impl L1HeadSubscription for NullL1HeadSubscription {
    fn take_stream(&mut self) -> BoxStream<'static, Result<u64, SourceError>> {
        futures::stream::pending().boxed()
    }
}
