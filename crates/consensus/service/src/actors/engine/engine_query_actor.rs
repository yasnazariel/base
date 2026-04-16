//! The [`EngineQueryActor`].

use std::sync::Arc;

use async_trait::async_trait;
use base_consensus_engine::{EngineClient, EngineState};
use base_consensus_genesis::RollupConfig;
use derive_more::Constructor;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use crate::{EngineError, EngineRpcRequest, NodeActor, actors::CancellableContext};

/// Processes engine RPC queries on a dedicated channel, independent of the [`crate::EngineActor`]
/// pipeline.
#[derive(Constructor, Debug)]
pub struct EngineQueryActor<EngineClient_: EngineClient> {
    engine_client: Arc<EngineClient_>,
    rollup_config: Arc<RollupConfig>,
    engine_state_receiver: watch::Receiver<EngineState>,
    engine_queue_length_receiver: watch::Receiver<usize>,
    cancellation_token: CancellationToken,
}

impl<EngineClient_: EngineClient> CancellableContext for EngineQueryActor<EngineClient_> {
    fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.cancellation_token.cancelled()
    }
}

impl<EngineClient_> EngineQueryActor<EngineClient_>
where
    EngineClient_: EngineClient + 'static,
{
    async fn handle_rpc_request(&self, request: EngineRpcRequest) -> Result<(), EngineError> {
        match request {
            EngineRpcRequest::EngineQuery(req) => {
                trace!(target: "engine", ?req, "Received engine query.");

                if let Err(e) = req
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
            }
        }

        Ok(())
    }
}

#[async_trait]
impl<EngineClient_> NodeActor for EngineQueryActor<EngineClient_>
where
    EngineClient_: EngineClient + 'static,
{
    type Error = EngineError;
    type StartData = mpsc::Receiver<EngineRpcRequest>;

    async fn start(self, mut request_channel: Self::StartData) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    info!(target: "engine", "EngineQueryActor shutting down.");
                    return Ok(());
                }
                req = request_channel.recv() => {
                    let Some(query) = req else {
                        error!(target: "engine", "Engine query request channel closed unexpectedly");
                        return Err(EngineError::ChannelClosed);
                    };
                    self.handle_rpc_request(query).await?;
                }
            }
        }
    }
}
