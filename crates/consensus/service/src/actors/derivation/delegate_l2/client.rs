use std::fmt::Debug;

use alloy_consensus::Block;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::B256;
use alloy_provider::{Provider, RootProvider};
use async_trait::async_trait;
use base_common_consensus::BaseTxEnvelope;
use base_common_network::Base;
use base_common_rpc_types_engine::{BaseExecutionPayload, BaseExecutionPayloadEnvelope};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// Error type for [`DelegateL2Client`] operations.
#[derive(Debug, Error)]
pub enum DelegateL2ClientError {
    /// Failed to fetch block from L2 EL.
    #[error("failed to fetch block at {tag}: {source}")]
    FetchBlock {
        /// The block tag that was requested.
        tag: String,
        /// The underlying transport error.
        source: alloy_transport::TransportError,
    },

    /// Block not found at the requested tag.
    #[error("block not found at {0}")]
    BlockNotFound(String),
}

// Private deserialization helper for the `debug_proofsSyncStatus` RPC response.
#[derive(Debug, Deserialize)]
struct ProofsSyncStatus {
    latest: Option<u64>,
}

/// Trait for querying the local L2 execution layer node.
///
/// Abstracts over the real [`RootProvider`] in production and an in-memory
/// implementation in action tests, allowing [`DelegateL2DerivationActor`] to be
/// driven without a live RPC connection.
///
/// [`DelegateL2DerivationActor`]: crate::DelegateL2DerivationActor
#[async_trait]
pub trait LocalL2Provider: Debug + Send + Sync {
    /// Returns the current block number of the local L2 execution layer.
    async fn block_number(&self) -> Result<u64, alloy_transport::TransportError>;

    /// Returns the block hash at the given block number, or `None` if the block
    /// is unknown or the RPC call fails.
    ///
    /// **`None` is treated as agreement** by callers: the safe-head
    /// fork-divergence check in `SyncFromSourceTask::update_safe_and_finalized`
    /// only skips a `SafeDB` write when a *known* hash mismatch is detected. If
    /// the local engine has not yet executed the block at `n`, this should
    /// return `None` so the caller does not incorrectly classify the chains as
    /// diverged.
    async fn block_hash_at(&self, n: u64) -> Option<B256>;

    /// Returns the latest block number known to the proofs `ExEx`, or `None` if
    /// proofs are not yet available. Returns an error if the RPC call fails.
    async fn proofs_latest_block(&self) -> Result<Option<u64>, alloy_transport::TransportError>;
}

#[async_trait]
impl LocalL2Provider for RootProvider<Base> {
    async fn block_number(&self) -> Result<u64, alloy_transport::TransportError> {
        Provider::get_block_number(self).await
    }

    async fn block_hash_at(&self, n: u64) -> Option<B256> {
        self.get_block_by_number(n.into()).await.ok()?.map(|b| b.header.hash)
    }

    async fn proofs_latest_block(&self) -> Result<Option<u64>, alloy_transport::TransportError> {
        let status: ProofsSyncStatus =
            self.raw_request("debug_proofsSyncStatus".into(), ()).await?;
        Ok(status.latest)
    }
}

/// Trait for fetching L2 block data from a source node.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait L2SourceClient: Debug + Send + Sync {
    /// Fetches the block number at the given tag.
    async fn get_block_number(&self, tag: BlockNumberOrTag) -> Result<u64, DelegateL2ClientError>;

    /// Fetches a block by number and converts it to an [`BaseExecutionPayloadEnvelope`].
    async fn get_payload_by_number(
        &self,
        number: u64,
    ) -> Result<BaseExecutionPayloadEnvelope, DelegateL2ClientError>;
}

/// Client that polls a source L2 execution layer node for block data and
/// converts blocks into [`BaseExecutionPayloadEnvelope`] for engine insertion.
#[derive(Debug, Clone)]
pub struct DelegateL2Client {
    provider: RootProvider<Base>,
}

impl DelegateL2Client {
    /// Creates a new [`DelegateL2Client`] from a source L2 node URL.
    pub fn new(url: Url) -> Self {
        let provider = RootProvider::<Base>::new_http(url);
        Self { provider }
    }
}

#[async_trait]
impl L2SourceClient for DelegateL2Client {
    async fn get_block_number(&self, tag: BlockNumberOrTag) -> Result<u64, DelegateL2ClientError> {
        let block = self
            .provider
            .get_block_by_number(tag)
            .await
            .map_err(|e| DelegateL2ClientError::FetchBlock { tag: format!("{tag:?}"), source: e })?
            .ok_or_else(|| DelegateL2ClientError::BlockNotFound(format!("{tag:?}")))?;

        Ok(block.header.number)
    }

    async fn get_payload_by_number(
        &self,
        number: u64,
    ) -> Result<BaseExecutionPayloadEnvelope, DelegateL2ClientError> {
        let rpc_block = self
            .provider
            .get_block_by_number(number.into())
            .full()
            .await
            .map_err(|e| DelegateL2ClientError::FetchBlock { tag: format!("{number}"), source: e })?
            .ok_or_else(|| DelegateL2ClientError::BlockNotFound(format!("{number}")))?;

        let block_hash = rpc_block.header.hash;
        let parent_beacon_block_root = rpc_block.header.parent_beacon_block_root;

        let txs: Vec<BaseTxEnvelope> = rpc_block
            .transactions
            .into_transactions()
            .map(|tx| tx.inner.inner.into_inner())
            .collect();

        let consensus_block: Block<BaseTxEnvelope> = Block {
            header: rpc_block.header.inner,
            body: alloy_consensus::BlockBody {
                transactions: txs,
                ommers: vec![],
                withdrawals: rpc_block.withdrawals,
            },
        };

        let (execution_payload, _sidecar) =
            BaseExecutionPayload::from_block_unchecked(block_hash, &consensus_block);

        Ok(BaseExecutionPayloadEnvelope { parent_beacon_block_root, execution_payload })
    }
}
