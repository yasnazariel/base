//! Consumer trait implementations for [`EngineHandle`].
//!
//! Implements [`SequencerEngineClient`], [`DerivationEngineClient`], and
//! [`NetworkEngineClient`] directly on `EngineHandle<C>`, eliminating the
//! need for separate `Queued*` wrapper types.

use alloy_rpc_types_engine::PayloadId;
use async_trait::async_trait;
use base_common_rpc_types_engine::BaseExecutionPayloadEnvelope;
use base_protocol::{AttributesWithParent, L2BlockInfo};

use super::EngineHandle;
use crate::{ConsolidateInput, EngineClient};

// ── Re-export the error/result types that consumer traits use ──

/// The result of an Engine client call.
pub type EngineClientResult<T> = Result<T, EngineClientError>;

/// Error making requests to the engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineClientError {
    /// Error making a request to the engine. The request never made it there.
    #[error("Error making a request to the engine: {0}.")]
    RequestError(String),

    /// Error receiving response from the engine.
    #[error("Error receiving response from the engine: {0}.")]
    ResponseError(String),

    /// An error occurred starting to build a block.
    #[error(transparent)]
    StartBuildError(#[from] crate::BuildTaskError),

    /// An error occurred sealing a block.
    #[error(transparent)]
    SealError(#[from] crate::SealTaskError),

    /// An error occurred inserting a block.
    #[error(transparent)]
    InsertError(#[from] crate::InsertTaskError),

    /// An error occurred consolidating.
    #[error(transparent)]
    ConsolidateError(#[from] crate::ConsolidateTaskError),

    /// An error occurred finalizing.
    #[error(transparent)]
    FinalizeError(#[from] crate::FinalizeTaskError),

    /// An error occurred performing the reset.
    #[error(transparent)]
    ResetError(#[from] crate::EngineResetError),
}

// ── SequencerEngineClient ──

/// Trait to be used by the Sequencer to interact with the engine, abstracting
/// the communication mechanism.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait SequencerEngineClient: std::fmt::Debug + Send + Sync {
    /// Resets the engine's forkchoice, awaiting confirmation that it succeeded or
    /// returning the error in performing the reset.
    async fn reset_engine_forkchoice(&self) -> EngineClientResult<()>;

    /// Starts building a block with the provided attributes.
    ///
    /// Returns a `PayloadId` that can be used to seal the block later.
    async fn start_build_block(
        &self,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<PayloadId>;

    /// Fetches the sealed payload envelope from the engine WITHOUT inserting it.
    /// Call this before attempting conductor commit, then call `insert_unsafe_payload` on
    /// success.
    async fn get_sealed_payload(
        &self,
        payload_id: PayloadId,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<BaseExecutionPayloadEnvelope>;

    /// Fire-and-forget: submits the sealed payload to the engine for insertion
    /// (`new_payload` + FCU). Call this after a successful conductor commit.
    async fn insert_unsafe_payload(
        &self,
        payload: BaseExecutionPayloadEnvelope,
    ) -> EngineClientResult<()>;

    /// Returns the current unsafe head [`L2BlockInfo`].
    async fn get_unsafe_head(&self) -> EngineClientResult<L2BlockInfo>;
}

/// Blanket implementation so `Arc<T>` can be used wherever `T: SequencerEngineClient`.
#[async_trait]
impl<T: SequencerEngineClient> SequencerEngineClient for std::sync::Arc<T> {
    async fn reset_engine_forkchoice(&self) -> EngineClientResult<()> {
        (**self).reset_engine_forkchoice().await
    }
    async fn start_build_block(
        &self,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<PayloadId> {
        (**self).start_build_block(attributes).await
    }
    async fn get_sealed_payload(
        &self,
        payload_id: PayloadId,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<BaseExecutionPayloadEnvelope> {
        (**self).get_sealed_payload(payload_id, attributes).await
    }
    async fn insert_unsafe_payload(
        &self,
        payload: BaseExecutionPayloadEnvelope,
    ) -> EngineClientResult<()> {
        (**self).insert_unsafe_payload(payload).await
    }
    async fn get_unsafe_head(&self) -> EngineClientResult<L2BlockInfo> {
        (**self).get_unsafe_head().await
    }
}

#[async_trait]
impl<C: EngineClient + std::fmt::Debug + 'static> SequencerEngineClient for EngineHandle<C> {
    async fn reset_engine_forkchoice(&self) -> EngineClientResult<()> {
        self.reset().await.map(|_| ()).map_err(Into::into)
    }

    async fn start_build_block(
        &self,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<PayloadId> {
        self.build(attributes).await.map_err(Into::into)
    }

    async fn get_sealed_payload(
        &self,
        payload_id: PayloadId,
        attributes: AttributesWithParent,
    ) -> EngineClientResult<BaseExecutionPayloadEnvelope> {
        self.get_payload(payload_id, attributes).await.map_err(Into::into)
    }

    async fn insert_unsafe_payload(
        &self,
        payload: BaseExecutionPayloadEnvelope,
    ) -> EngineClientResult<()> {
        self.insert(payload, false).await.map_err(Into::into)
    }

    async fn get_unsafe_head(&self) -> EngineClientResult<L2BlockInfo> {
        Ok(self.state().sync_state.unsafe_head())
    }
}

// ── DerivationEngineClient ──

/// Client to use to interact with the engine from the derivation actor.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock(type SafeL2Signal = AttributesWithParent;))]
#[async_trait]
pub trait DerivationEngineClient: std::fmt::Debug + Send + Sync {
    /// Resets the engine's forkchoice.
    async fn reset_engine_forkchoice(&self) -> EngineClientResult<()>;

    /// Sends a request to finalize the L2 block at the provided block number.
    /// Note: This does not wait for the engine to process it.
    async fn send_finalized_l2_block(&self, block_number: u64) -> EngineClientResult<()>;

    /// Sends a consolidation signal to the engine.
    async fn send_safe_l2_signal(&self, signal: ConsolidateInput) -> EngineClientResult<()>;
}

#[async_trait]
impl<C: EngineClient + std::fmt::Debug + 'static> DerivationEngineClient for EngineHandle<C> {
    async fn reset_engine_forkchoice(&self) -> EngineClientResult<()> {
        self.reset().await.map(|_| ()).map_err(Into::into)
    }

    async fn send_finalized_l2_block(&self, block_number: u64) -> EngineClientResult<()> {
        self.finalize(block_number).await.map_err(Into::into)
    }

    async fn send_safe_l2_signal(&self, signal: ConsolidateInput) -> EngineClientResult<()> {
        self.consolidate(signal).await.map_err(Into::into)
    }
}

// ── NetworkEngineClient ──

/// Client used to interact with the Engine from the network actor.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait NetworkEngineClient: std::fmt::Debug + Send + Sync {
    /// Note: a successful response does not mean the block was successfully inserted.
    /// This function just sends the message to the engine. It does not wait for a response.
    async fn send_unsafe_block(
        &self,
        block: BaseExecutionPayloadEnvelope,
    ) -> EngineClientResult<()>;
}

#[async_trait]
impl<C: EngineClient + std::fmt::Debug + 'static> NetworkEngineClient for EngineHandle<C> {
    async fn send_unsafe_block(
        &self,
        block: BaseExecutionPayloadEnvelope,
    ) -> EngineClientResult<()> {
        self.insert(block, false).await.map_err(Into::into)
    }
}
