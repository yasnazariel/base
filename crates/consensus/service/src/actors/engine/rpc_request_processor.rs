//! RPC request processor for engine queries.

use std::sync::Arc;

use base_consensus_engine::{EngineClient, EngineQueries, EngineState};
use base_consensus_genesis::RollupConfig;
use derive_more::Constructor;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use super::EngineError;

/// Requires that the implementor handles [`EngineQueries`] via the provided channel.
pub trait EngineRpcRequestReceiver: Send + Sync {
    /// Starts a task to handle engine queries.
    fn start(
        self,
        request_channel: mpsc::Receiver<EngineQueries>,
    ) -> JoinHandle<Result<(), EngineError>>;
}

/// Processor for engine RPC queries.
#[derive(Constructor, Debug)]
pub struct EngineRpcProcessor<EngineClient_: EngineClient> {
    /// An [`EngineClient`] used for creating engine tasks.
    engine_client: Arc<EngineClient_>,
    /// The [`RollupConfig`] used to build tasks.
    rollup_config: Arc<RollupConfig>,
    /// Receiver for [`EngineState`] updates.
    engine_state_receiver: watch::Receiver<EngineState>,
    /// Receiver for engine queue length updates.
    engine_queue_length_receiver: watch::Receiver<usize>,
}

impl<EngineClient_> EngineRpcProcessor<EngineClient_>
where
    EngineClient_: EngineClient + 'static,
{
    async fn handle_query(&self, query: EngineQueries) -> Result<(), EngineError> {
        trace!(target: "engine", ?query, "Received engine query.");

        if let Err(e) = query
            .handle(
                &self.engine_state_receiver,
                &self.engine_queue_length_receiver,
                &self.engine_client,
                &self.rollup_config,
            )
            .await
        {
            warn!(target: "engine", err = ?e, "Failed to handle engine query.");
        }

        Ok(())
    }
}

impl<EngineClient_> EngineRpcRequestReceiver for EngineRpcProcessor<EngineClient_>
where
    EngineClient_: EngineClient + 'static,
{
    fn start(
        self,
        mut request_channel: mpsc::Receiver<EngineQueries>,
    ) -> JoinHandle<Result<(), EngineError>> {
        tokio::spawn(async move {
            loop {
                let Some(query) = request_channel.recv().await else {
                    error!(target: "engine", "Engine rpc request receiver closed unexpectedly");
                    return Err(EngineError::ChannelClosed);
                };
                self.handle_query(query).await?;
            }
        })
    }
}
