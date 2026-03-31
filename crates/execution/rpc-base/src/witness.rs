//! Support for Base-specific witness RPCs.

use std::{fmt::Debug, sync::Arc};

use alloy_primitives::B256;
use alloy_rpc_types_debug::ExecutionWitness;
use base_alloy_chains::BaseUpgrades;
use base_execution_payload_builder::{
    OpPayloadAttributes, OpPayloadBuilder, OpPayloadBuilderAttributes,
};
use jsonrpsee_core::{RpcResult, async_trait};
use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_evm::ConfigureEvm;
use reth_payload_primitives::BuildNextEnv;
use reth_primitives::{OpHeader, OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::SealedHeader;
pub use reth_rpc_api::DebugExecutionWitnessApiServer;
use reth_rpc_server_types::{ToRpcResult, result::internal_rpc_err};
use reth_storage_api::{BlockReaderIdExt, NodePrimitivesProvider, StateProviderFactory};
use reth_storage_errors::provider::{ProviderError, ProviderResult};
use reth_tasks::TaskSpawner;
use reth_transaction_pool::{OpPooledTx, TransactionPool};
use tokio::sync::{Semaphore, oneshot};

/// An extension to the `debug_` namespace of the RPC API.
pub struct OpDebugWitnessApi<Pool, Provider, EvmConfig> {
    inner: Arc<OpDebugWitnessApiInner<Pool, Provider, EvmConfig>>,
}

impl<Pool, Provider, EvmConfig> OpDebugWitnessApi<Pool, Provider, EvmConfig> {
    /// Creates a new instance of the `OpDebugWitnessApi`.
    pub fn new(
        provider: Provider,
        task_spawner: Box<dyn TaskSpawner>,
        builder: OpPayloadBuilder<Pool, Provider, EvmConfig>,
    ) -> Self {
        let semaphore = Arc::new(Semaphore::new(3));
        let inner = OpDebugWitnessApiInner { provider, builder, task_spawner, semaphore };
        Self { inner: Arc::new(inner) }
    }
}

impl<Pool, Provider, EvmConfig> OpDebugWitnessApi<Pool, Provider, EvmConfig>
where
    EvmConfig: ConfigureEvm,
    Provider:
        NodePrimitivesProvider<Primitives = OpPrimitives> + BlockReaderIdExt<Header = OpHeader>,
{
    /// Fetches the parent header by hash.
    fn parent_header(&self, parent_block_hash: B256) -> ProviderResult<SealedHeader<OpHeader>> {
        self.inner
            .provider
            .sealed_header_by_hash(parent_block_hash)?
            .ok_or_else(|| ProviderError::HeaderNotFound(parent_block_hash.into()))
    }
}

#[async_trait]
impl<Pool, Provider, EvmConfig> DebugExecutionWitnessApiServer<OpPayloadAttributes>
    for OpDebugWitnessApi<Pool, Provider, EvmConfig>
where
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTransactionSigned>> + 'static,
    Provider: BlockReaderIdExt<Header = OpHeader>
        + NodePrimitivesProvider<Primitives = OpPrimitives>
        + StateProviderFactory
        + ChainSpecProvider<ChainSpec: EthChainSpec<Header = OpHeader> + BaseUpgrades>
        + Clone
        + 'static,
    EvmConfig: ConfigureEvm<
            Primitives = OpPrimitives,
            NextBlockEnvCtx: BuildNextEnv<
                OpPayloadBuilderAttributes,
                OpHeader,
                Provider::ChainSpec,
            >,
        > + 'static,
{
    async fn execute_payload(
        &self,
        parent_block_hash: B256,
        attributes: OpPayloadAttributes,
    ) -> RpcResult<ExecutionWitness> {
        let _permit = self.inner.semaphore.acquire().await;

        let parent_header = self.parent_header(parent_block_hash).to_rpc_result()?;

        let (tx, rx) = oneshot::channel();
        let this = self.clone();
        self.inner.task_spawner.spawn_blocking_task(Box::pin(async move {
            let res = this.inner.builder.payload_witness(parent_header, attributes);
            let _ = tx.send(res);
        }));

        rx.await
            .map_err(|err| internal_rpc_err(err.to_string()))?
            .map_err(|err| internal_rpc_err(err.to_string()))
    }
}

impl<Pool, Provider, EvmConfig> Clone for OpDebugWitnessApi<Pool, Provider, EvmConfig> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<Pool, Provider, EvmConfig> Debug for OpDebugWitnessApi<Pool, Provider, EvmConfig> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpDebugWitnessApi").finish_non_exhaustive()
    }
}

struct OpDebugWitnessApiInner<Pool, Provider, EvmConfig> {
    provider: Provider,
    builder: OpPayloadBuilder<Pool, Provider, EvmConfig>,
    task_spawner: Box<dyn TaskSpawner>,
    semaphore: Arc<Semaphore>,
}
