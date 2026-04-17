use std::sync::Arc;

use base_builder_publish::WebSocketPublisher;
use base_execution_evm::BaseEvmConfig;
use base_node_core::{
    BaseConsensusBuilder, BaseExecutorBuilder, BaseNetworkBuilder, node::BasePoolBuilder,
};
use base_node_runner::{
    BaseNode, BaseNodeTypes, PayloadServiceBuilder as BasePayloadServiceBuilder,
};
use derive_more::Debug;
use reth_engine_primitives::TreeConfig;
use reth_engine_tree::tree::{PayloadProcessor, precompile_cache::PrecompileCacheMap};
use reth_node_api::NodeTypes;
use reth_node_builder::{
    BuilderContext,
    components::{ComponentsBuilder, PayloadServiceBuilder},
};
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_provider::CanonStateSubscriptions;
use reth_trie_db::ChangesetCache;
use tracing::info;

use super::{
    PayloadHandler, generator::BlockPayloadJobGenerator, payload::BasePayloadBuilder,
    state_root_task::StateRootTaskDeps,
};
use crate::{
    BuilderConfig,
    traits::{NodeBounds, PoolBounds},
};

/// Builder for the flashblocks payload service.
///
/// Wraps [`BuilderConfig`] and implements [`BasePayloadServiceBuilder`] to spawn
/// the flashblocks payload builder service, which produces sub-block chunks
/// (flashblocks) at sub-second intervals during block construction.
#[derive(Debug)]
pub struct FlashblocksServiceBuilder(pub BuilderConfig);

impl FlashblocksServiceBuilder {
    fn spawn_payload_builder_service<Node, Pool>(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>
    where
        Node: NodeBounds,
        Pool: PoolBounds,
    {
        let (built_payload_tx, built_payload_rx) = tokio::sync::mpsc::channel(16);

        let ws_pub: Arc<WebSocketPublisher> =
            WebSocketPublisher::new(self.0.flashblocks_ws_addr)?.into();

        // PayloadProcessor is reused across blocks for warm sparse trie.
        let evm_config = BaseEvmConfig::base(ctx.chain_spec());
        // Production uses default TreeConfig (cache pruning ON). The
        // state_root benchmark disables pruning to keep the trie warm across
        // iterations — that is intentional and not meant to mirror production.
        let tree_config = TreeConfig::default();
        let changeset_cache = ChangesetCache::new();
        let runtime = reth_tasks::Runtime::with_existing_handle(tokio::runtime::Handle::current())?;
        let payload_processor = PayloadProcessor::new(
            runtime,
            evm_config.clone(),
            &tree_config,
            PrecompileCacheMap::default(),
        );

        let state_root_deps =
            StateRootTaskDeps::new(payload_processor, changeset_cache, tree_config);

        let payload_builder = BasePayloadBuilder::new(
            evm_config,
            pool,
            ctx.provider().clone(),
            self.0.clone(),
            built_payload_tx,
            ws_pub,
            state_root_deps,
        );
        let payload_generator = BlockPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_builder,
            true,
            self.0.block_time_leeway,
        );

        let (payload_service, payload_builder_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        let payload_handler =
            PayloadHandler::new(built_payload_rx, payload_service.payload_events_handle());

        ctx.task_executor()
            .spawn_critical_task("custom payload builder service", Box::pin(payload_service));
        ctx.task_executor()
            .spawn_critical_task("flashblocks payload handler", Box::pin(payload_handler.run()));

        info!("Flashblocks payload builder service started");
        Ok(payload_builder_handle)
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool, BaseEvmConfig> for FlashblocksServiceBuilder
where
    Node: NodeBounds,
    Pool: PoolBounds,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        _: BaseEvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>> {
        self.spawn_payload_builder_service(ctx, pool)
    }
}

impl BasePayloadServiceBuilder for FlashblocksServiceBuilder {
    type ComponentsBuilder = ComponentsBuilder<
        BaseNodeTypes,
        BasePoolBuilder,
        Self,
        BaseNetworkBuilder,
        BaseExecutorBuilder,
        BaseConsensusBuilder,
    >;

    fn build_components(self, base_node: &BaseNode) -> Self::ComponentsBuilder {
        base_node.components::<BaseNodeTypes>().payload(self)
    }
}
