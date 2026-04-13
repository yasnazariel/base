//! Queue-based RPC client that sends engine queries directly to the RPC processor.

use std::fmt::Debug;

use alloy_eips::BlockNumberOrTag;
use async_trait::async_trait;
use base_consensus_engine::{EngineQueries, EngineState};
use base_consensus_genesis::RollupConfig;
use base_consensus_rpc::EngineRpcClient;
use base_protocol::{L2BlockInfo, OutputRoot};
use derive_more::Constructor;
use jsonrpsee::{
    core::RpcResult,
    types::{ErrorCode, ErrorObject},
};
use tokio::sync::{mpsc, oneshot, watch};

/// Queue-based implementation of the [`EngineRpcClient`] trait.
///
/// Sends [`EngineQueries`] directly to the RPC processor channel.
#[derive(Clone, Constructor, Debug)]
pub struct QueuedEngineRpcClient {
    /// A channel to send queries directly to the RPC processor.
    pub rpc_query_tx: mpsc::Sender<EngineQueries>,
}

#[async_trait]
impl EngineRpcClient for QueuedEngineRpcClient {
    async fn get_config(&self) -> RpcResult<RollupConfig> {
        let (config_tx, config_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::Config(config_tx))
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        config_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive config from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }

    async fn get_state(&self) -> RpcResult<EngineState> {
        let (state_tx, state_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::State(state_tx))
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        state_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive state from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }

    async fn output_at_block(
        &self,
        block: BlockNumberOrTag,
    ) -> RpcResult<(L2BlockInfo, OutputRoot, EngineState)> {
        let (output_tx, output_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::OutputAtBlock { block, sender: output_tx })
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        output_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive output at block from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }

    async fn dev_get_task_queue_length(&self) -> RpcResult<usize> {
        let (length_tx, length_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::TaskQueueLength(length_tx))
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        length_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive task queue length from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }

    async fn dev_subscribe_to_engine_queue_length(&self) -> RpcResult<watch::Receiver<usize>> {
        let (sub_tx, sub_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::QueueLengthReceiver(sub_tx))
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        sub_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive queue length receiver from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }

    async fn dev_subscribe_to_engine_state(&self) -> RpcResult<watch::Receiver<EngineState>> {
        let (sub_tx, sub_rx) = oneshot::channel();

        self.rpc_query_tx
            .send(EngineQueries::StateReceiver(sub_tx))
            .await
            .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;

        sub_rx.await.map_err(|_| {
            error!(target: "block_engine", "Failed to receive state receiver from engine rpc");
            ErrorObject::from(ErrorCode::InternalError)
        })
    }
}
