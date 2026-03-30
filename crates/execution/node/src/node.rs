//! Base Node types config.

use std::{marker::PhantomData, sync::Arc};

use base_alloy_chains::BaseUpgrades;
use base_alloy_consensus::OpPooledTransaction;
use base_alloy_rpc_types_engine::OpExecutionData;
use base_execution_consensus::OpBeaconConsensus;
use base_execution_evm::{OpEvmConfig, OpRethReceiptBuilder};
use base_execution_payload_builder::{
    OpPayloadBuilderAttributes, OpPayloadPrimitives,
    builder::OpPayloadTransactions,
    config::{OpBuilderConfig, OpDAConfig, OpGasLimitConfig},
};
use base_execution_rpc::{
    config::{BaseEthConfigApiServer, BaseEthConfigHandler},
    eth::OpEthApiBuilder,
    miner::{MinerApiExtServer, OpMinerExtApi},
    witness::{DebugExecutionWitnessApiServer, OpDebugWitnessApi},
};
use base_execution_storage::OpStorage;
use base_txpool::{
    BaseOrdering, BasePooledTransaction, OpPooledTx, OpTransactionPool, OpTransactionValidator,
    TimestampedTransaction,
};
use reth_chainspec::{ChainSpec, ChainSpecProvider, EthChainSpec, Hardforks};
use reth_evm::ConfigureEvm;
use reth_network::{
    NetworkConfig, NetworkHandle, NetworkManager, NetworkPrimitives, PeersInfo,
    types::BasicNetworkPrimitives,
};
use reth_node_api::{
    AddOnsContext, BuildNextEnv, EngineTypes, FullNodeComponents, NodeAddOns, PrimitivesTy, TxTy,
};
use reth_node_builder::{
    BuilderContext, Node, NodeAdapter, NodeComponentsBuilder,
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ConsensusBuilder, ExecutorBuilder,
        NetworkBuilder, PayloadBuilderBuilder, PoolBuilder, PoolBuilderConfigOverrides,
        TxPoolBuilder,
    },
    node::{FullNodeTypes, NodeTypes},
    rpc::{
        BasicEngineValidatorBuilder, EngineApiBuilder, EngineValidatorAddOn,
        EngineValidatorBuilder, EthApiBuilder, Identity, PayloadValidatorBuilder, RethRpcAddOns,
        RethRpcMiddleware, RethRpcServerHandles, RpcAddOns, RpcContext, RpcHandle,
    },
};
use reth_primitives::{OpHeader, OpPrimitives, OpTransactionSigned};
use reth_provider::providers::ProviderFactoryBuilder;
use reth_rpc_api::{DebugApiServer, eth::RpcTypes};
use reth_rpc_server_types::RethRpcModule;
use reth_tracing::tracing::{debug, info};
use reth_transaction_pool::{
    EthPoolTransaction, PoolPooledTx, PoolTransaction, TransactionPool,
    TransactionValidationTaskExecutor, blobstore::DiskFileBlobStore,
};
use reth_trie_common::KeccakKeyHasher;

use crate::{
    OpEngineApiBuilder, OpEngineTypes,
    args::{RollupArgs, TxpoolOrdering},
    engine::OpEngineValidator,
};

/// Marker trait for Base node types with standard engine, chain spec, and primitives.
pub trait OpNodeTypes:
    NodeTypes<Payload = OpEngineTypes, ChainSpec = ChainSpec, Primitives = OpPrimitives>
{
}
/// Blanket impl for all node types that conform to the Base spec.
impl<N> OpNodeTypes for N where
    N: NodeTypes<Payload = OpEngineTypes, ChainSpec = ChainSpec, Primitives = OpPrimitives>
{
}

/// Helper trait for Base node types with full configuration including storage and execution
/// data.
pub trait OpFullNodeTypes:
    NodeTypes<
        ChainSpec = ChainSpec,
        Primitives: OpPayloadPrimitives,
        Storage = OpStorage,
        Payload: EngineTypes<ExecutionData = OpExecutionData>,
    >
{
}

impl<N> OpFullNodeTypes for N where
    N: NodeTypes<
            ChainSpec = ChainSpec,
            Primitives: OpPayloadPrimitives,
            Storage = OpStorage,
            Payload: EngineTypes<ExecutionData = OpExecutionData>,
        >
{
}

/// Type configuration for a regular Base node.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct OpNode {
    /// Additional Base args
    pub args: RollupArgs,
    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the OP builder.
    /// Used to control the gas limit of the blocks produced by the OP builder.(configured by the
    /// batcher via the `miner_` api)
    pub gas_limit_config: OpGasLimitConfig,
}

/// A [`ComponentsBuilder`] with its generic arguments set to a stack of Base-specific builders.
pub type OpNodeComponentBuilder<Node, Payload = OpPayloadBuilder> = ComponentsBuilder<
    Node,
    OpPoolBuilder,
    BasicPayloadServiceBuilder<Payload>,
    OpNetworkBuilder,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

impl OpNode {
    /// Creates a new instance of the Base node type.
    pub fn new(args: RollupArgs) -> Self {
        Self {
            args,
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
        }
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Configure the gas limit configuration for the OP builder.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.gas_limit_config = gas_limit_config;
        self
    }

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(&self) -> OpNodeComponentBuilder<Node>
    where
        Node: FullNodeTypes<Types: OpNodeTypes>,
    {
        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            txpool_ordering,
            ..
        } = self.args;
        let ordering = match txpool_ordering {
            TxpoolOrdering::CoinbaseTip => BaseOrdering::coinbase_tip(),
            TxpoolOrdering::Timestamp => BaseOrdering::timestamp(),
        };
        ComponentsBuilder::default()
            .node_types::<Node>()
            .executor(OpExecutorBuilder::default())
            .pool(OpPoolBuilder::default().with_ordering(ordering))
            .payload(BasicPayloadServiceBuilder::new(
                OpPayloadBuilder::new(compute_pending_block)
                    .with_da_config(self.da_config.clone())
                    .with_gas_limit_config(self.gas_limit_config.clone()),
            ))
            .network(OpNetworkBuilder::new(disable_txpool_gossip, !discovery_v4))
            .consensus(OpConsensusBuilder::default())
    }

    /// Returns [`OpAddOnsBuilder`] with configured arguments.
    pub fn add_ons_builder<NetworkT: RpcTypes>(&self) -> OpAddOnsBuilder<NetworkT> {
        OpAddOnsBuilder::default()
            .with_sequencer(self.args.sequencer.clone())
            .with_sequencer_headers(self.args.sequencer_headers.clone())
            .with_da_config(self.da_config.clone())
            .with_gas_limit_config(self.gas_limit_config.clone())
            .with_min_suggested_priority_fee(self.args.min_suggested_priority_fee)
    }

    /// Instantiates the [`ProviderFactoryBuilder`] for an opstack node.
    ///
    /// # Open a Providerfactory in read-only mode from a datadir
    ///
    /// See also: [`ProviderFactoryBuilder`] and
    /// [`ReadOnlyConfig`](reth_provider::providers::ReadOnlyConfig).
    ///
    /// ```no_run
    /// use reth_chainspec::BASE_MAINNET;
    /// use base_node_core::OpNode;
    ///
    /// fn demo(runtime: reth_tasks::Runtime) {
    ///     let factory = OpNode::provider_factory_builder()
    ///         .open_read_only(BASE_MAINNET.clone(), "datadir", runtime)
    ///         .unwrap();
    /// }
    /// ```
    ///
    /// # Open a Providerfactory with custom config
    ///
    /// ```no_run
    /// use reth_chainspec::ChainSpecBuilder;
    /// use base_node_core::OpNode;
    /// use reth_provider::providers::ReadOnlyConfig;
    ///
    /// fn demo(runtime: reth_tasks::Runtime) {
    ///     let factory = OpNode::provider_factory_builder()
    ///         .open_read_only(
    ///             ChainSpecBuilder::base_mainnet().build().into(),
    ///             ReadOnlyConfig::from_datadir("datadir").no_watch(),
    ///             runtime,
    ///         )
    ///         .unwrap();
    /// }
    /// ```
    pub fn provider_factory_builder() -> ProviderFactoryBuilder<Self> {
        ProviderFactoryBuilder::default()
    }
}

impl<N> Node<N> for OpNode
where
    N: FullNodeTypes<Types: OpFullNodeTypes + OpNodeTypes>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        OpPoolBuilder,
        BasicPayloadServiceBuilder<OpPayloadBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        OpEthApiBuilder,
        OpEngineValidatorBuilder,
        OpEngineApiBuilder<OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self)
    }

    fn add_ons(&self) -> Self::AddOns {
        self.add_ons_builder().build()
    }
}

impl NodeTypes for OpNode {
    type Primitives = OpPrimitives;
    type ChainSpec = ChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

/// Add-ons w.r.t. Base.
///
/// This type provides Base-specific addons to the node and exposes the RPC server and engine
/// API.
#[derive(Debug)]
pub struct OpAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB,
    EB = OpEngineApiBuilder<PVB>,
    EVB = BasicEngineValidatorBuilder<PVB>,
    RpcMiddleware = Identity,
> {
    /// Rpc add-ons responsible for launching the RPC servers and instantiating the RPC handlers
    /// and eth-api.
    pub rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
    /// Data availability configuration for the OP builder.
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the OP builder.
    pub gas_limit_config: OpGasLimitConfig,
    /// Sequencer client, configured to forward submitted transactions to sequencer of given OP
    /// network.
    pub sequencer_url: Option<String>,
    /// Headers to use for the sequencer client requests.
    pub sequencer_headers: Vec<String>,
    min_suggested_priority_fee: u64,
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Creates a new instance from components.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
        sequencer_url: Option<String>,
        sequencer_headers: Vec<String>,
        min_suggested_priority_fee: u64,
    ) -> Self {
        Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
        }
    }
}

impl<N> Default for OpAddOns<N, OpEthApiBuilder, OpEngineValidatorBuilder>
where
    N: FullNodeComponents<Types: OpNodeTypes>,
    OpEthApiBuilder: EthApiBuilder<N>,
{
    fn default() -> Self {
        Self::builder().build()
    }
}

impl<N, NetworkT, RpcMiddleware>
    OpAddOns<
        N,
        OpEthApiBuilder<NetworkT>,
        OpEngineValidatorBuilder,
        OpEngineApiBuilder<OpEngineValidatorBuilder>,
        RpcMiddleware,
    >
where
    N: FullNodeComponents<Types: OpNodeTypes>,
    OpEthApiBuilder<NetworkT>: EthApiBuilder<N>,
{
    /// Build a [`OpAddOns`] using [`OpAddOnsBuilder`].
    pub fn builder() -> OpAddOnsBuilder<NetworkT> {
        OpAddOnsBuilder::default()
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Maps the [`reth_node_builder::rpc::EngineApiBuilder`] builder type.
    pub fn with_engine_api<T>(
        self,
        engine_api_builder: T,
    ) -> OpAddOns<N, EthB, PVB, T, EVB, RpcMiddleware> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
            ..
        } = self;
        OpAddOns::new(
            rpc_add_ons.with_engine_api(engine_api_builder),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
        )
    }

    /// Maps the [`PayloadValidatorBuilder`] builder type.
    pub fn with_payload_validator<T>(
        self,
        payload_validator_builder: T,
    ) -> OpAddOns<N, EthB, T, EB, EVB, RpcMiddleware> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
            ..
        } = self;
        OpAddOns::new(
            rpc_add_ons.with_payload_validator(payload_validator_builder),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
        )
    }

    /// Sets the RPC middleware stack for processing RPC requests.
    ///
    /// This method configures a custom middleware stack that will be applied to all RPC requests
    /// across HTTP, `WebSocket`, and IPC transports. The middleware is applied to the RPC service
    /// layer, allowing you to intercept, modify, or enhance RPC request processing.
    ///
    /// See also [`RpcAddOns::with_rpc_middleware`].
    pub fn with_rpc_middleware<T>(self, rpc_middleware: T) -> OpAddOns<N, EthB, PVB, EB, EVB, T> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
            ..
        } = self;
        OpAddOns::new(
            rpc_add_ons.with_rpc_middleware(rpc_middleware),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
        )
    }

    /// Sets the hook that is run once the rpc server is started.
    pub fn on_rpc_started<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(RpcContext<'_, N, EthB::EthApi>, RethRpcServerHandles) -> eyre::Result<()>
            + Send
            + 'static,
    {
        self.rpc_add_ons = self.rpc_add_ons.on_rpc_started(hook);
        self
    }

    /// Sets the hook that is run to configure the rpc modules.
    pub fn extend_rpc_modules<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(RpcContext<'_, N, EthB::EthApi>) -> eyre::Result<()> + Send + 'static,
    {
        self.rpc_add_ons = self.rpc_add_ons.extend_rpc_modules(hook);
        self
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> NodeAddOns<N>
    for OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
            Types: OpNodeTypes,
            Evm: ConfigureEvm<
                Primitives = OpPrimitives,
                NextBlockEnvCtx: BuildNextEnv<OpPayloadBuilderAttributes, OpHeader, ChainSpec>,
            >,
            Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTransactionSigned>>,
        >,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
{
    type Handle = RpcHandle<N, EthB::EthApi>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        let Self { rpc_add_ons, da_config, gas_limit_config, .. } = self;
        let eth_config =
            BaseEthConfigHandler::new(ctx.node.provider().clone(), ctx.node.evm_config().clone());

        let builder = base_execution_payload_builder::OpPayloadBuilder::new(
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
            ctx.node.evm_config().clone(),
        );
        // install additional OP specific rpc methods
        let debug_ext = OpDebugWitnessApi::<_, _, _>::new(
            ctx.node.provider().clone(),
            Box::new(ctx.node.task_executor().clone()),
            builder,
        );
        let miner_ext = OpMinerExtApi::new(da_config, gas_limit_config);

        rpc_add_ons
            .launch_add_ons_with(ctx, move |container| {
                let reth_node_builder::rpc::RpcModuleContainer { modules, auth_module, registry } =
                    container;

                modules.merge_if_module_configured(RethRpcModule::Eth, eth_config.into_rpc())?;

                debug!(target: "reth::cli", "Installing debug payload witness rpc endpoint");
                modules.merge_if_module_configured(RethRpcModule::Debug, debug_ext.into_rpc())?;

                // extend the miner namespace if configured in the regular http server
                modules.add_or_replace_if_module_configured(
                    RethRpcModule::Miner,
                    miner_ext.clone().into_rpc(),
                )?;

                // install the miner extension in the authenticated if configured
                if modules.module_config().contains_any(&RethRpcModule::Miner) {
                    debug!(target: "reth::cli", "Installing miner DA rpc endpoint");
                    auth_module.merge_auth_methods(miner_ext.into_rpc())?;
                }

                // install the debug namespace in the authenticated if configured
                if modules.module_config().contains_any(&RethRpcModule::Debug) {
                    debug!(target: "reth::cli", "Installing debug rpc endpoint");
                    auth_module.merge_auth_methods(registry.debug_api().into_rpc())?;
                }

                Ok(())
            })
            .await
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> RethRpcAddOns<N>
    for OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
            Types: OpNodeTypes,
            Evm: ConfigureEvm<
                Primitives = OpPrimitives,
                NextBlockEnvCtx: BuildNextEnv<OpPayloadBuilderAttributes, OpHeader, ChainSpec>,
            >,
        >,
    <<N as FullNodeComponents>::Pool as TransactionPool>::Transaction:
        OpPooledTx<Consensus = OpTransactionSigned>,
    EthB: EthApiBuilder<N>,
    PVB: PayloadValidatorBuilder<N>,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
{
    type EthApi = EthB::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.rpc_add_ons.hooks_mut()
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> EngineValidatorAddOn<N>
    for OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: Send,
{
    type ValidatorBuilder = EVB;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        EngineValidatorAddOn::engine_validator_builder(&self.rpc_add_ons)
    }
}

/// A regular Base EVM and executor builder.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OpAddOnsBuilder<NetworkT, RpcMiddleware = Identity> {
    /// Sequencer client, configured to forward submitted transactions to sequencer of given OP
    /// network.
    sequencer_url: Option<String>,
    /// Headers to use for the sequencer client requests.
    sequencer_headers: Vec<String>,
    /// Data availability configuration for the OP builder.
    da_config: Option<OpDAConfig>,
    /// Gas limit configuration for the OP builder.
    gas_limit_config: Option<OpGasLimitConfig>,
    /// Marker for network types.
    _nt: PhantomData<NetworkT>,
    /// Minimum suggested priority fee (tip)
    min_suggested_priority_fee: u64,
    /// RPC middleware to use
    rpc_middleware: RpcMiddleware,
    /// Optional tokio runtime to use for the RPC server.
    tokio_runtime: Option<tokio::runtime::Handle>,
}

impl<NetworkT> Default for OpAddOnsBuilder<NetworkT> {
    fn default() -> Self {
        Self {
            sequencer_url: None,
            sequencer_headers: Vec::new(),
            da_config: None,
            gas_limit_config: None,
            min_suggested_priority_fee: 1_000_000,
            _nt: PhantomData,
            rpc_middleware: Identity::new(),
            tokio_runtime: None,
        }
    }
}

impl<NetworkT, RpcMiddleware> OpAddOnsBuilder<NetworkT, RpcMiddleware> {
    /// With a [`SequencerClient`].
    pub fn with_sequencer(mut self, sequencer_client: Option<String>) -> Self {
        self.sequencer_url = sequencer_client;
        self
    }

    /// With headers to use for the sequencer client requests.
    pub fn with_sequencer_headers(mut self, sequencer_headers: Vec<String>) -> Self {
        self.sequencer_headers = sequencer_headers;
        self
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = Some(da_config);
        self
    }

    /// Configure the gas limit configuration for the OP payload builder.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.gas_limit_config = Some(gas_limit_config);
        self
    }

    /// Configure the minimum priority fee (tip)
    pub const fn with_min_suggested_priority_fee(mut self, min: u64) -> Self {
        self.min_suggested_priority_fee = min;
        self
    }

    /// Configures a custom tokio runtime for the RPC server.
    ///
    /// Caution: This runtime must not be created from within asynchronous context.
    pub fn with_tokio_runtime(mut self, tokio_runtime: Option<tokio::runtime::Handle>) -> Self {
        self.tokio_runtime = tokio_runtime;
        self
    }

    /// Configure the RPC middleware to use
    pub fn with_rpc_middleware<T>(self, rpc_middleware: T) -> OpAddOnsBuilder<NetworkT, T> {
        let Self {
            sequencer_url,
            sequencer_headers,
            da_config,
            gas_limit_config,
            min_suggested_priority_fee,
            tokio_runtime,
            _nt,
            ..
        } = self;
        OpAddOnsBuilder {
            sequencer_url,
            sequencer_headers,
            da_config,
            gas_limit_config,
            min_suggested_priority_fee,
            _nt,
            rpc_middleware,
            tokio_runtime,
        }
    }
}

impl<NetworkT, RpcMiddleware> OpAddOnsBuilder<NetworkT, RpcMiddleware> {
    /// Builds an instance of [`OpAddOns`].
    pub fn build<N, PVB, EB, EVB>(
        self,
    ) -> OpAddOns<N, OpEthApiBuilder<NetworkT>, PVB, EB, EVB, RpcMiddleware>
    where
        N: FullNodeComponents<Types: NodeTypes>,
        OpEthApiBuilder<NetworkT>: EthApiBuilder<N>,
        PVB: PayloadValidatorBuilder<N> + Default,
        EB: Default,
        EVB: Default,
    {
        let Self {
            sequencer_url,
            sequencer_headers,
            da_config,
            gas_limit_config,
            min_suggested_priority_fee,
            rpc_middleware,
            tokio_runtime,
            ..
        } = self;

        OpAddOns::new(
            RpcAddOns::new(
                OpEthApiBuilder::default()
                    .with_sequencer(sequencer_url.clone())
                    .with_sequencer_headers(sequencer_headers.clone())
                    .with_min_suggested_priority_fee(min_suggested_priority_fee),
                PVB::default(),
                EB::default(),
                EVB::default(),
                rpc_middleware,
            )
            .with_tokio_runtime(tokio_runtime),
            da_config.unwrap_or_default(),
            gas_limit_config.unwrap_or_default(),
            sequencer_url,
            sequencer_headers,
            min_suggested_priority_fee,
        )
    }
}

/// A regular Base EVM and executor builder.
#[derive(Debug, Copy, Clone, Default)]
#[non_exhaustive]
pub struct OpExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for OpExecutorBuilder
where
    Node: FullNodeTypes<Types: OpNodeTypes>,
{
    type EVM = OpEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let evm_config = OpEvmConfig::new(ctx.chain_spec(), OpRethReceiptBuilder::default());

        Ok(evm_config)
    }
}

/// A basic Base transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug)]
pub struct OpPoolBuilder<T = BasePooledTransaction> {
    /// Enforced overrides that are applied to the pool config.
    pub pool_config_overrides: PoolBuilderConfigOverrides,
    /// The ordering strategy for the transaction pool.
    pub ordering: BaseOrdering<T>,
    /// Marker for the pooled transaction type.
    _pd: core::marker::PhantomData<T>,
}

impl<T> Default for OpPoolBuilder<T> {
    fn default() -> Self {
        Self {
            pool_config_overrides: Default::default(),
            ordering: BaseOrdering::default(),
            _pd: Default::default(),
        }
    }
}

impl<T> Clone for OpPoolBuilder<T> {
    fn clone(&self) -> Self {
        Self {
            pool_config_overrides: self.pool_config_overrides.clone(),
            ordering: self.ordering.clone(),
            _pd: core::marker::PhantomData,
        }
    }
}

impl<T> OpPoolBuilder<T> {
    /// Sets the [`PoolBuilderConfigOverrides`] on the pool builder.
    pub fn with_pool_config_overrides(
        mut self,
        pool_config_overrides: PoolBuilderConfigOverrides,
    ) -> Self {
        self.pool_config_overrides = pool_config_overrides;
        self
    }

    /// Sets the ordering strategy for the transaction pool.
    pub const fn with_ordering(mut self, ordering: BaseOrdering<T>) -> Self {
        self.ordering = ordering;
        self
    }
}

impl<Node, T, Evm> PoolBuilder<Node, Evm> for OpPoolBuilder<T>
where
    Node: FullNodeTypes<Types: OpNodeTypes>,
    T: EthPoolTransaction<Consensus = TxTy<Node::Types>> + OpPooledTx + TimestampedTransaction,
    Evm: ConfigureEvm<Primitives = PrimitivesTy<Node::Types>> + Clone + 'static,
{
    type Pool = OpTransactionPool<Node::Provider, DiskFileBlobStore, Evm, T, BaseOrdering<T>>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: Evm,
    ) -> eyre::Result<Self::Pool> {
        let Self { pool_config_overrides, ordering, .. } = self;

        let blob_store = reth_node_builder::components::create_blob_store(ctx)?;
        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .no_eip4844()
                .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
                .kzg_settings(ctx.kzg_settings()?)
                .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
                .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
                .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
                .with_additional_tasks(
                    pool_config_overrides
                        .additional_validation_tasks
                        .unwrap_or_else(|| ctx.config().txpool.additional_validation_tasks),
                )
                .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
                .map(|validator| {
                    OpTransactionValidator::new(validator)
                        // In --dev mode we can't require gas fees because we're unable to decode
                        // the L1 block info
                        .require_l1_data_gas_fee(!ctx.config().dev.dev)
                });

        let final_pool_config = pool_config_overrides.apply(ctx.pool_config());

        let transaction_pool = TxPoolBuilder::new(ctx)
            .with_validator(validator)
            .build_with_ordering_and_spawn_maintenance_task(
                ordering,
                blob_store,
                final_pool_config,
            )?;

        info!(target: "reth::cli", "Transaction pool initialized");
        debug!(target: "reth::cli", "Spawned txpool maintenance task");

        Ok(transaction_pool)
    }
}

/// A basic Base payload service builder
#[derive(Debug, Default, Clone)]
pub struct OpPayloadBuilder<Txs = ()> {
    /// By default the pending block equals the latest block
    /// to save resources and not leak txs from the tx-pool,
    /// this flag enables computing of the pending block
    /// from the tx-pool instead.
    ///
    /// If `compute_pending_block` is not enabled, the payload builder
    /// will use the payload attributes from the latest block. Note
    /// that this flag is not yet functional.
    pub compute_pending_block: bool,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// This data availability configuration specifies constraints for the payload builder
    /// when assembling payloads
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the OP builder.
    /// This is used to configure gas limit related constraints for the payload builder.
    pub gas_limit_config: OpGasLimitConfig,
}

impl OpPayloadBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    pub fn new(compute_pending_block: bool) -> Self {
        Self {
            compute_pending_block,
            best_transactions: (),
            da_config: OpDAConfig::default(),
            gas_limit_config: OpGasLimitConfig::default(),
        }
    }

    /// Configure the data availability configuration for the OP payload builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Configure the gas limit configuration for the OP payload builder.
    pub fn with_gas_limit_config(mut self, gas_limit_config: OpGasLimitConfig) -> Self {
        self.gas_limit_config = gas_limit_config;
        self
    }
}

impl<Txs> OpPayloadBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> OpPayloadBuilder<T> {
        let Self { compute_pending_block, da_config, gas_limit_config, .. } = self;
        OpPayloadBuilder { compute_pending_block, best_transactions, da_config, gas_limit_config }
    }
}

impl<Node, Pool, Txs, Evm> PayloadBuilderBuilder<Node, Pool, Evm> for OpPayloadBuilder<Txs>
where
    Node: FullNodeTypes<Provider: ChainSpecProvider<ChainSpec: BaseUpgrades>, Types: OpNodeTypes>,
    Evm: ConfigureEvm<
            Primitives = OpPrimitives,
            NextBlockEnvCtx: BuildNextEnv<OpPayloadBuilderAttributes, OpHeader, ChainSpec>,
        > + 'static,
    Pool:
        TransactionPool<Transaction: OpPooledTx<Consensus = OpTransactionSigned>> + Unpin + 'static,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type PayloadBuilder =
        base_execution_payload_builder::OpPayloadBuilder<Pool, Node::Provider, Evm, Txs>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: Evm,
    ) -> eyre::Result<Self::PayloadBuilder> {
        let payload_builder =
            base_execution_payload_builder::OpPayloadBuilder::with_builder_config(
                pool,
                ctx.provider().clone(),
                evm_config,
                OpBuilderConfig {
                    da_config: self.da_config.clone(),
                    gas_limit_config: self.gas_limit_config.clone(),
                },
            )
            .with_transactions(self.best_transactions.clone())
            .set_compute_pending_block(self.compute_pending_block);
        Ok(payload_builder)
    }
}

/// A basic Base network builder.
#[derive(Debug, Default)]
pub struct OpNetworkBuilder {
    /// Disable transaction pool gossip
    pub disable_txpool_gossip: bool,
    /// Disable discovery v4
    pub disable_discovery_v4: bool,
}

impl Clone for OpNetworkBuilder {
    fn clone(&self) -> Self {
        Self::new(self.disable_txpool_gossip, self.disable_discovery_v4)
    }
}

impl OpNetworkBuilder {
    /// Creates a new `OpNetworkBuilder`.
    pub const fn new(disable_txpool_gossip: bool, disable_discovery_v4: bool) -> Self {
        Self { disable_txpool_gossip, disable_discovery_v4 }
    }
}

impl OpNetworkBuilder {
    /// Returns the [`NetworkConfig`] that contains the settings to launch the p2p network.
    ///
    /// This applies the configured [`OpNetworkBuilder`] settings.
    pub fn network_config<Node, NetworkP>(
        &self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<NetworkConfig<Node::Provider, NetworkP>>
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
        NetworkP: NetworkPrimitives,
    {
        let disable_txpool_gossip = self.disable_txpool_gossip;
        let disable_discovery_v4 = self.disable_discovery_v4;
        let args = &ctx.config().network;
        let network_builder = ctx
            .network_config_builder()?
            // apply discovery settings
            .apply(|mut builder| {
                let rlpx_socket = (args.addr, args.port).into();
                if disable_discovery_v4 || args.discovery.disable_discovery {
                    builder = builder.disable_discv4_discovery();
                }
                if !args.discovery.disable_discovery {
                    builder = builder.discovery_v5(
                        args.discovery.discovery_v5_builder(
                            rlpx_socket,
                            ctx.config()
                                .network
                                .resolved_bootnodes()
                                .or_else(|| ctx.chain_spec().bootnodes())
                                .unwrap_or_default(),
                        ),
                    );
                }

                builder
            });

        let mut network_config = ctx.build_network_config(network_builder);

        // When `sequencer_endpoint` is configured, the node will forward all transactions to a
        // Sequencer node for execution and inclusion on L1, and disable its own txpool
        // gossip to prevent other parties in the network from learning about them.
        network_config.tx_gossip_disabled = disable_txpool_gossip;

        Ok(network_config)
    }
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for OpNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    type Network =
        NetworkHandle<BasicNetworkPrimitives<PrimitivesTy<Node::Types>, PoolPooledTx<Pool>>>;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        let network_config = self.network_config(ctx)?;
        let network = NetworkManager::builder(network_config).await?;
        let handle = ctx.start_network(network, pool);
        info!(target: "reth::cli", enode=%handle.local_node_record(), "P2P networking initialized");

        Ok(handle)
    }
}

/// A basic Base consensus builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct OpConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for OpConsensusBuilder
where
    Node: FullNodeTypes<Types: OpNodeTypes>,
{
    type Consensus = Arc<OpBeaconConsensus>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(OpBeaconConsensus::new(ctx.chain_spec())))
    }
}

/// Builder for [`OpEngineValidator`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct OpEngineValidatorBuilder;

impl<Node> PayloadValidatorBuilder<Node> for OpEngineValidatorBuilder
where
    Node: FullNodeComponents<Types: OpNodeTypes>,
{
    type Validator = OpEngineValidator<Node::Provider>;

    async fn build(self, ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(OpEngineValidator::new::<KeccakKeyHasher>(
            Arc::clone(&ctx.config.chain),
            ctx.node.provider().clone(),
        ))
    }
}

/// Network primitive types used by Base networks.
pub type OpNetworkPrimitives = BasicNetworkPrimitives<OpPrimitives, OpPooledTransaction>;
