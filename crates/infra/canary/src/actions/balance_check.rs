//! Balance check canary action.

use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256, utils::format_ether};
use alloy_provider::{Provider, ProviderBuilder};
use async_trait::async_trait;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;

use crate::{ActionOutcome, CanaryAction, Metrics};

/// Timeout for individual RPC calls.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Checks the canary wallet balance and warns when it drops below a threshold.
#[derive(Debug, Clone)]
pub struct BalanceCheckAction {
    l2_rpc_url: Url,
    address: Address,
    min_balance_wei: U256,
}

impl BalanceCheckAction {
    /// Creates a new [`BalanceCheckAction`].
    pub const fn new(l2_rpc_url: Url, address: Address, min_balance_wei: U256) -> Self {
        Self { l2_rpc_url, address, min_balance_wei }
    }
}

#[async_trait]
impl CanaryAction for BalanceCheckAction {
    fn name(&self) -> &'static str {
        "balance_check"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        let provider = ProviderBuilder::new().connect_http(self.l2_rpc_url.clone());

        let balance = tokio::select! {
            () = cancel.cancelled() => return ActionOutcome::failed("cancelled", start),
            result = timeout(RPC_TIMEOUT, provider.get_balance(self.address)) => match result {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return ActionOutcome::failed(format!("failed to fetch balance: {e}"), start),
                Err(_) => return ActionOutcome::failed(
                    format!("balance fetch timed out after {}s", RPC_TIMEOUT.as_secs()),
                    start,
                ),
            }
        };

        let balance_eth = format_ether(balance);
        debug!(balance_wei = %balance, balance_eth = %balance_eth, "fetched canary wallet balance");

        // NOTE: f64 loses precision above 2^53 wei (~9 ETH). A canary wallet
        // is expected to hold well under that, so the approximation is acceptable.
        Metrics::wallet_balance_wei().set(balance.to::<u128>() as f64);

        if balance >= self.min_balance_wei {
            ActionOutcome::success(format!("balance {balance_eth} ETH is above minimum"), start)
        } else {
            let min_eth = format_ether(self.min_balance_wei);
            warn!(
                balance_eth = %balance_eth,
                min_eth = %min_eth,
                "canary wallet balance below minimum"
            );
            ActionOutcome::failed(
                format!("balance {balance_eth} ETH is below minimum {min_eth} ETH"),
                start,
            )
        }
    }
}
