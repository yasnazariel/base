//! Invalid batch submission canary action.

use std::time::{Duration, Instant};

use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::Bytes;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use async_trait::async_trait;
use base_consensus_rpc::RollupNodeApiClient;
use base_protocol::{DERIVATION_VERSION_0, Frame};
use jsonrpsee::http_client::HttpClientBuilder;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use url::Url;

use crate::{ActionOutcome, CanaryAction};

/// Timeout for the rollup config RPC call.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout waiting for the L1 receipt.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// Submits a batch transaction to the L1 batch inbox signed with the wrong key.
///
/// The L1 accepts the transaction (it is a valid signed tx), but the derivation
/// pipeline rejects it because the signer does not match the registered batcher
/// address in the rollup config.
#[derive(Debug)]
pub struct InvalidBatchAction {
    l1_rpc_url: Url,
    cl_rpc_url: Url,
    signer: PrivateKeySigner,
}

impl InvalidBatchAction {
    /// Creates a new [`InvalidBatchAction`].
    pub const fn new(l1_rpc_url: Url, cl_rpc_url: Url, signer: PrivateKeySigner) -> Self {
        Self { l1_rpc_url, cl_rpc_url, signer }
    }
}

#[async_trait]
impl CanaryAction for InvalidBatchAction {
    fn name(&self) -> &'static str {
        "invalid_batch"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        // Fetch rollup config to get the batch inbox address and L1 chain ID.
        let cl_client = match HttpClientBuilder::default().build(self.cl_rpc_url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                return ActionOutcome::failed(format!("failed to build CL RPC client: {e}"), start);
            }
        };

        let rollup_config = tokio::select! {
            () = cancel.cancelled() => return ActionOutcome::failed("cancelled", start),
            result = timeout(CONFIG_TIMEOUT, cl_client.rollup_config()) => match result {
                Ok(Ok(cfg)) => cfg,
                Ok(Err(e)) => return ActionOutcome::failed(
                    format!("rollup_config RPC failed: {e}"),
                    start,
                ),
                Err(_) => return ActionOutcome::failed("rollup_config RPC timed out", start),
            }
        };

        let batch_inbox = rollup_config.batch_inbox_address;
        let l1_chain_id = rollup_config.l1_chain_id;
        debug!(inbox = %batch_inbox, l1_chain_id, "fetched rollup config for invalid batch action");

        // Build a minimal frame: all-zero channel ID, frame 0, tiny payload, last frame.
        let frame = Frame::new([0u8; 16], 0, b"base-canary-invalid".to_vec(), true);
        let mut calldata = vec![DERIVATION_VERSION_0];
        calldata.extend_from_slice(&frame.encode());

        let wallet = EthereumWallet::from(self.signer.clone());
        let provider = ProviderBuilder::new().wallet(wallet).connect_http(self.l1_rpc_url.clone());

        let tx = TransactionRequest::default()
            .with_to(batch_inbox)
            .with_input(Bytes::from(calldata))
            .with_chain_id(l1_chain_id);

        let pending = tokio::select! {
            () = cancel.cancelled() => return ActionOutcome::failed("cancelled", start),
            result = provider.send_transaction(tx) => match result {
                Ok(p) => p,
                Err(e) => return ActionOutcome::failed(
                    format!("failed to send L1 batch tx: {e}"),
                    start,
                ),
            }
        };

        let tx_hash = *pending.tx_hash();
        info!(tx_hash = %tx_hash, inbox = %batch_inbox, "invalid batch tx submitted to L1");

        let receipt = tokio::select! {
            () = cancel.cancelled() => {
                return ActionOutcome::failed(
                    format!("cancelled after submission (tx: {tx_hash})"),
                    start,
                );
            }
            result = timeout(RECEIPT_TIMEOUT, pending.get_receipt()) => match result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return ActionOutcome::failed(
                    format!("receipt fetch failed: {e}"),
                    start,
                ),
                Err(_) => return ActionOutcome::failed(
                    format!("receipt timed out after {}s (tx: {tx_hash})", RECEIPT_TIMEOUT.as_secs()),
                    start,
                ),
            }
        };

        let block_number = receipt.block_number.unwrap_or(0);
        ActionOutcome::success(
            format!("invalid batch tx {tx_hash} mined in L1 block {block_number}"),
            start,
        )
    }
}
