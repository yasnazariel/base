//! Health check canary action.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, ProviderBuilder};
use async_trait::async_trait;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::{ActionOutcome, CanaryAction, Metrics};

/// Timeout for individual RPC calls.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Fetches the latest L2 block and checks whether its age exceeds a threshold.
#[derive(Debug, Clone)]
pub struct HealthCheckAction {
    l2_rpc_url: Url,
    max_block_age: Duration,
}

impl HealthCheckAction {
    /// Creates a new [`HealthCheckAction`].
    pub const fn new(l2_rpc_url: Url, max_block_age: Duration) -> Self {
        Self { l2_rpc_url, max_block_age }
    }
}

#[async_trait]
impl CanaryAction for HealthCheckAction {
    fn name(&self) -> &'static str {
        "health_check"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        let provider = ProviderBuilder::new().connect_http(self.l2_rpc_url.clone());

        let block = tokio::select! {
            () = cancel.cancelled() => return ActionOutcome::failed("cancelled", start),
            result = timeout(RPC_TIMEOUT, provider.get_block_by_number(BlockNumberOrTag::Latest)) => {
                match result {
                    Ok(Ok(Some(b))) => b,
                    Ok(Ok(None)) => return ActionOutcome::failed("latest block not found", start),
                    Ok(Err(e)) => return ActionOutcome::failed(
                        format!("failed to fetch latest block: {e}"),
                        start,
                    ),
                    Err(_) => return ActionOutcome::failed(
                        format!("block fetch timed out after {}s", RPC_TIMEOUT.as_secs()),
                        start,
                    ),
                }
            }
        };

        let now_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(e) => return ActionOutcome::failed(format!("system clock error: {e}"), start),
        };

        let block_timestamp = block.header.timestamp;
        let block_age_secs = now_secs.saturating_sub(block_timestamp);
        let block_age = Duration::from_secs(block_age_secs);

        debug!(
            block_number = block.header.number,
            block_timestamp = block_timestamp,
            block_age_secs = block_age_secs,
            "fetched latest block"
        );

        Metrics::health_check_block_age_ms().set(block_age.as_millis() as f64);

        if block_age <= self.max_block_age {
            ActionOutcome::success(
                format!(
                    "block {} is {block_age_secs}s old (threshold: {}s)",
                    block.header.number,
                    self.max_block_age.as_secs()
                ),
                start,
            )
        } else {
            ActionOutcome::failed(
                format!(
                    "block {} is {block_age_secs}s old, exceeds threshold of {}s",
                    block.header.number,
                    self.max_block_age.as_secs()
                ),
                start,
            )
        }
    }
}
