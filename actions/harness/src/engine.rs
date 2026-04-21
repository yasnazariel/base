//! In-process engine client for action tests backed by the production `BasePayloadBuilder`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use alloy_consensus::{BlockHeader, Header, Sealed, transaction::Recovered};
use alloy_eips::{BlockId, eip1898::BlockNumberOrTag, eip2718::Encodable2718};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::{Ethereum, Network};
use alloy_primitives::{Address, B256, BlockHash, StorageKey, U256};
use alloy_provider::{EthGetBlock, ProviderCall, RpcWithBlock};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadEnvelopeV2, ExecutionPayloadInputV2,
    ExecutionPayloadV1, ExecutionPayloadV3, ForkchoiceState, ForkchoiceUpdated, PayloadId,
    PayloadStatus, PayloadStatusEnum,
};
use alloy_rpc_types_eth::{
    Block, BlockTransactions, EIP1186AccountProofResponse, Transaction as EthTransaction,
};
use alloy_transport::{TransportError, TransportErrorKind, TransportResult};
use async_trait::async_trait;
use base_common_consensus::{BaseBlock, BaseBlockBody, BasePrimitives, BaseTxEnvelope};
use base_common_network::Base;
use base_common_provider::BaseEngineApi;
use base_common_rpc_types::Transaction as BaseTransaction;
use base_common_rpc_types_engine::{
    BaseExecutionPayload, BaseExecutionPayloadEnvelope, BaseExecutionPayloadEnvelopeV3,
    BaseExecutionPayloadEnvelopeV4, BaseExecutionPayloadEnvelopeV5, BaseExecutionPayloadV4,
    BasePayloadAttributes,
};
use base_consensus_engine::{EngineClient, EngineClientError};
use base_consensus_genesis::RollupConfig;
use base_consensus_node::{EngineClientError as NodeEngineClientError, SequencerEngineClient};
use base_execution_chainspec::BaseChainSpec;
use base_execution_evm::BaseEvmConfig;
use base_execution_payload_builder::{
    BaseBuiltPayload, BasePayloadBuilder, BasePayloadBuilderAttributes,
};
use base_execution_txpool::BasePooledTransaction;
use base_node_core::BaseNode;
use base_protocol::{AttributesWithParent, BlockInfo, L2BlockInfo};
use base_test_utils::build_test_genesis;
use reth_basic_payload_builder::{
    BuildArguments, PayloadBuilder as RethPayloadBuilder, PayloadConfig,
};
use reth_db::{DatabaseEnv, test_utils::TempDatabase};
use reth_db_common::init::init_genesis;
use reth_execution_types::ExecutionOutcome;
use reth_node_api::NodeTypesWithDBAdapter;
use reth_payload_primitives::{BuiltPayload, PayloadBuilderAttributes};
use reth_primitives_traits::{SealedBlock, SealedHeader};
use reth_provider::{
    BlockExecutionWriter, BlockNumReader, BlockWriter, HashedPostStateProvider,
    LatestStateProviderRef, ProviderFactory, StageCheckpointWriter, StateProvider,
    StateProviderFactory, providers::BlockchainProvider,
    test_utils::create_test_provider_factory_with_node_types,
};
use reth_stages_types::{StageCheckpoint, StageId};
use reth_transaction_pool::noop::NoopTransactionPool;

use crate::{SharedBlockHashRegistry, SharedL1Chain};

/// Type alias for the node type adapter used in tests.
pub type TestNodeTypes = NodeTypesWithDBAdapter<BaseNode, Arc<TempDatabase<DatabaseEnv>>>;

/// Type alias for the test provider factory used by the engine client.
pub type TestProviderFactory = ProviderFactory<TestNodeTypes>;

/// Type alias for the test blockchain provider used by the engine client.
pub type TestBlockchainProvider = BlockchainProvider<TestNodeTypes>;

/// Type alias for the noop pool used by the engine client.
pub type TestPool = NoopTransactionPool<BasePooledTransaction>;

/// A payload built in-process during sequencer mode, waiting to be fetched via `get_payload`.
#[derive(Debug, Clone)]
pub struct PendingPayload {
    /// The built payload from the production `BasePayloadBuilder`.
    pub built: BaseBuiltPayload<BasePrimitives>,
}

/// Mutable state owned by [`ActionEngineClient`], protected by a `Mutex` so
/// the client can implement the `&self` methods required by [`EngineClient`].
#[derive(Debug)]
pub struct ActionEngineClientInner {
    /// The raw provider factory for committing blocks to the database.
    provider_factory: TestProviderFactory,
    /// The blockchain provider wrapping the same factory, used by the builder
    /// since it implements `StateProviderFactory`.
    blockchain_provider: TestBlockchainProvider,
    evm_config: BaseEvmConfig,
    chain_spec: Arc<BaseChainSpec>,
    canonical_head: L2BlockInfo,
    executed_headers: HashMap<u64, Header>,
    /// Decoded transactions for each executed block, keyed by block number.
    ///
    /// Stored alongside headers so that `get_l2_block` can return full blocks with
    /// transaction bodies. `find_starting_forkchoice` calls `L2BlockInfo::from_block_and_genesis`
    /// on the unsafe head, which requires the L1 info deposit tx (first transaction) to be
    /// present; header-only blocks cause `MissingL1InfoDeposit` errors.
    executed_transactions: HashMap<u64, Vec<BaseTxEnvelope>>,
    /// Current safe head, updated by `fork_choice_updated_vX` when `safe_block_hash` is
    /// non-zero. Mirrors what the production engine tracks for the safe label.
    safe_head: L2BlockInfo,
    /// Current finalized head, updated by `fork_choice_updated_vX` when
    /// `finalized_block_hash` is non-zero.
    finalized_head: L2BlockInfo,
    /// Payloads built via FCU-with-attrs (sequencer mode), keyed by `PayloadId`.
    pending_payloads: HashMap<PayloadId, PendingPayload>,
    payload_counter: u64,
}

/// An in-process engine client for action tests backed by the production `BasePayloadBuilder`.
///
/// `ActionEngineClient` implements [`EngineClient`] using the real Reth payload building
/// code path (`BasePayloadBuilder::try_build`). It supports two workflows:
///
/// ## Derivation mode
///
/// When `new_payload_vX` is called with a derived payload, every transaction is
/// executed through the production builder, the resulting state root is computed, and — when
/// the paired `block_registry` contains a sequencer-computed state root for that
/// block — the two roots are asserted equal. A mismatch panics with a diagnostic
/// showing both roots and the block number.
///
/// ## Sequencer mode
///
/// When `fork_choice_updated_vX` is called with `payload_attributes`, transactions
/// are executed via the production builder, a `PayloadId` is returned, and the resulting payload
/// is stored pending retrieval via `get_payload_vX`. A subsequent `new_payload` call
/// with the same block is a no-op (the EVM state was already advanced during the
/// build step), ensuring the builder is not applied twice.
///
/// [`L2Sequencer`]: crate::L2Sequencer
#[derive(Clone, Debug)]
pub struct ActionEngineClient {
    inner: Arc<Mutex<ActionEngineClientInner>>,
    rollup_config: Arc<RollupConfig>,
    block_registry: SharedBlockHashRegistry,
    l1_chain: SharedL1Chain,
}

impl ActionEngineClient {
    /// Build a test genesis whose hardfork schedule and chain ID match `rollup_config`.
    ///
    /// Starts from [`build_test_genesis`] (pre-funded test accounts, all forks through
    /// Jovian at timestamp 0) and overrides each fork timestamp and the chain ID from the
    /// rollup config so the resulting [`BaseChainSpec`] matches the test's expectations.
    ///
    /// The genesis block timestamp is set to `rollup_config.genesis.l2_time` so that
    /// `L2BlockInfo::from_block_and_genesis` on the Reth genesis block returns a timestamp
    /// matching the rollup config. Without this, `confirmed_safe_head.timestamp` would be
    /// taken from the Reth genesis block's timestamp (which defaults to 1 in
    /// [`build_test_genesis`]), causing batch timestamp checks to use `next_timestamp =
    /// genesis_ts + block_time` that is higher than the first sequenced batch's timestamp,
    /// dropping it as `PastTimestampPreHolocene`.
    fn build_genesis_for_rollup(rollup_config: &RollupConfig) -> Genesis {
        let mut genesis = build_test_genesis();
        genesis.config.chain_id = rollup_config.l2_chain_id.id();
        genesis.timestamp = rollup_config.genesis.l2_time;

        // Fund the harness test account. `build_test_genesis` only funds the Anvil accounts
        // (Alice/Bob/Charlie/Deployer); `TEST_ACCOUNT_ADDRESS` is separate and must be seeded
        // here so transactions signed by this account have enough ETH to pay gas.
        let test_balance = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
        genesis.alloc.insert(
            crate::TEST_ACCOUNT_ADDRESS,
            GenesisAccount::default().with_balance(test_balance),
        );

        // Compute effective (cascaded) fork timestamps. `RollupConfig::is_X_active` cascades
        // upward: e.g. `is_ecotone_active` returns true if fjord is active, regardless of whether
        // `ecotone_time` is set. The Reth chain spec has no cascade — each fork must be explicitly
        // set. We mirror the cascade here so the EVM hardfork schedule matches the pipeline's view
        // and `new_payload_vN` version selection (which calls `is_ecotone_active` etc.) is
        // consistent with what the Reth EVM accepts. Use `min` semantics: if an upper fork
        // activates earlier than the lower fork's explicit time, the upper fork determines the
        // effective lower-fork activation (e.g. fjord_time=0 with ecotone_time=6 → eff_ecotone=0).
        let hf = &rollup_config.hardforks;
        let eff = |explicit: Option<u64>, implied_by: Option<u64>| -> Option<u64> {
            match (explicit, implied_by) {
                (Some(e), Some(i)) => Some(e.min(i)),
                (Some(e), None) => Some(e),
                (None, i) => i,
            }
        };
        let eff_jovian = hf.jovian_time;
        let eff_isthmus = eff(hf.isthmus_time, eff_jovian);
        let eff_holocene = eff(hf.holocene_time, eff_isthmus);
        let eff_granite = eff(hf.granite_time, eff_holocene);
        let eff_fjord = eff(hf.fjord_time, eff_granite);
        let eff_ecotone = eff(hf.ecotone_time, eff_fjord);
        let eff_canyon = eff(hf.canyon_time, eff_ecotone);
        let eff_regolith = eff(hf.regolith_time, eff_canyon);

        // Helper: set or clear a JSON extra-field that BaseChainSpec::from_genesis reads.
        macro_rules! set_ts {
            ($key:expr, $val:expr) => {
                match $val {
                    Some(ts) => {
                        genesis.config.extra_fields.insert($key.to_string(), serde_json::json!(ts));
                    }
                    None => {
                        genesis.config.extra_fields.remove($key);
                    }
                }
            };
        }
        set_ts!("regolithTime", eff_regolith);
        set_ts!("canyonTime", eff_canyon);
        set_ts!("ecotoneTime", eff_ecotone);
        set_ts!("fjordTime", eff_fjord);
        set_ts!("graniteTime", eff_granite);
        set_ts!("holoceneTime", eff_holocene);
        set_ts!("isthmusTime", eff_isthmus);
        set_ts!("jovianTime", eff_jovian);

        // V1 requires Osaka (the EL counterpart). Both must be set together.
        match hf.base.azul {
            Some(ts) => {
                genesis.config.osaka_time = Some(ts);
                genesis
                    .config
                    .extra_fields
                    .insert("base".to_string(), serde_json::json!({ "azul": ts }));
            }
            None => {
                genesis.config.osaka_time = None;
                genesis.config.extra_fields.remove("base");
            }
        }

        genesis
    }

    /// Compute the L2 genesis block hash for the given rollup config.
    ///
    /// The Reth DB stores genesis under its real computed hash, not `B256::ZERO`.
    /// This method returns that hash so callers can set `rollup_config.genesis.l2.hash`
    /// to the real value before creating derivation components, ensuring the pipeline's
    /// `l2_safe_head.hash` matches the `parent_hash` encoded in batches by the sequencer.
    pub fn compute_l2_genesis_hash(rollup_config: &RollupConfig) -> B256 {
        let chain_spec =
            Arc::new(BaseChainSpec::from_genesis(Self::build_genesis_for_rollup(rollup_config)));
        chain_spec.genesis_header().hash_slow()
    }

    /// Create a new `ActionEngineClient` backed by a production payload builder.
    ///
    /// Initializes a temporary Reth database with the test genesis state and creates
    /// a production `BasePayloadBuilder` for block building.
    pub fn new(
        rollup_config: Arc<RollupConfig>,
        canonical_head: L2BlockInfo,
        block_registry: SharedBlockHashRegistry,
        l1_chain: SharedL1Chain,
    ) -> Self {
        // Build a genesis whose chain ID and hardfork schedule matches the rollup config.
        // build_test_genesis() provides the genesis accounts and sets all forks through
        // Jovian at timestamp 0; we override per-fork times and the chain ID here.
        let chain_spec =
            Arc::new(BaseChainSpec::from_genesis(Self::build_genesis_for_rollup(&rollup_config)));
        let provider_factory =
            create_test_provider_factory_with_node_types::<BaseNode>(Arc::clone(&chain_spec));
        init_genesis(&provider_factory).expect("failed to initialize genesis in action engine");
        let blockchain_provider = BlockchainProvider::new(provider_factory.clone())
            .expect("failed to create blockchain provider");
        let evm_config = BaseEvmConfig::base(Arc::clone(&chain_spec));

        // Pre-populate the genesis block header so `get_l2_block(genesis_hash)` lookups
        // during `find_starting_forkchoice` resolve to the real genesis instead of `None`.
        // Without this, the engine reset triggered after the first unsafe block is inserted
        // (`el_sync_finished` transitions from false to true) fails with `BlockNotFound`.
        let genesis_header = chain_spec.genesis_header().clone();
        let genesis_block_number = genesis_header.number;
        let mut initial_executed_headers = HashMap::new();
        initial_executed_headers.insert(genesis_block_number, genesis_header);
        // Genesis has no transactions; the `from_block_and_genesis` genesis fast path does not
        // require any, so an empty vec is correct here.
        let mut initial_executed_transactions: HashMap<u64, Vec<BaseTxEnvelope>> = HashMap::new();
        initial_executed_transactions.insert(genesis_block_number, vec![]);

        let inner = Arc::new(Mutex::new(ActionEngineClientInner {
            provider_factory,
            blockchain_provider,
            evm_config,
            chain_spec,
            canonical_head,
            safe_head: canonical_head,
            finalized_head: canonical_head,
            executed_headers: initial_executed_headers,
            executed_transactions: initial_executed_transactions,
            pending_payloads: HashMap::new(),
            payload_counter: 0,
        }));
        Self { inner, rollup_config, block_registry, l1_chain }
    }

    /// Return the current canonical (unsafe) head tracked by this engine client.
    ///
    /// Reads the inner mutex synchronously — safe to call outside an async context
    /// and without awaiting, making it suitable for tight assertion loops in tests.
    pub fn unsafe_head(&self) -> L2BlockInfo {
        self.inner.lock().expect("action engine inner lock poisoned").canonical_head
    }

    /// Return the current safe head tracked by this engine client.
    ///
    /// Updated each time `fork_choice_updated_vX` is called with a non-zero
    /// `safe_block_hash`. Initial value equals the genesis head.
    pub fn safe_head(&self) -> L2BlockInfo {
        self.inner.lock().expect("action engine inner lock poisoned").safe_head
    }

    /// Return the current finalized head tracked by this engine client.
    ///
    /// Updated each time `fork_choice_updated_vX` is called with a non-zero
    /// `finalized_block_hash`. Initial value equals the genesis head.
    pub fn finalized_head(&self) -> L2BlockInfo {
        self.inner.lock().expect("action engine inner lock poisoned").finalized_head
    }

    /// Directly reset the tracked finalized head to `head`.
    ///
    /// Used by [`crate::TestActorDerivationNode::act_reset`] to perform a
    /// synchronous finalized-head reset. In production the finalized head is
    /// only advanced via `fork_choice_updated_vX`; resetting it is a
    /// test-only operation.
    pub fn reset_finalized_head(&self, head: L2BlockInfo) {
        self.inner.lock().expect("action engine inner lock poisoned").finalized_head = head;
    }

    /// Reset all tracked heads and clear any committed blocks above `safe_head`.
    ///
    /// Used by [`crate::TestActorDerivationNode::act_reset`] to mirror the full
    /// engine-state reset that happens on a pipeline reset: all three head labels
    /// are set to `safe_head`, and every block committed above `safe_head.number`
    /// is purged from both the in-memory caches (`executed_headers`/
    /// `executed_transactions`) and the Reth DB, so that `build_and_commit` can
    /// recommit fresh blocks at the same heights after a reorg without hitting
    /// duplicate-key errors on the `BlockBodyIndices` append cursor.
    pub fn reset_engine_state(&self, safe_head: L2BlockInfo) {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        guard.canonical_head = safe_head;
        guard.safe_head = safe_head;
        guard.finalized_head = safe_head;
        let keep_up_to = safe_head.block_info.number;
        guard.executed_headers.retain(|&n, _| n <= keep_up_to);
        guard.executed_transactions.retain(|&n, _| n <= keep_up_to);

        // Unwind the Reth DB to match the reset point.
        //
        // `build_and_commit` uses cursor.append which fails if the block already
        // exists in the DB. `init_genesis` sets all stage checkpoints to block 0,
        // but committed blocks don't update them, so `unwind_trie_state_from` would
        // compute an empty range. We fix this by first syncing the Finish checkpoint
        // to the actual DB tip before calling `remove_block_and_execution_above`.
        let provider_rw = guard
            .provider_factory
            .provider_rw()
            .expect("reset_engine_state: failed to get provider_rw");
        let last_block = provider_rw.last_block_number().unwrap_or(0);
        if last_block > keep_up_to {
            provider_rw
                .save_stage_checkpoint(StageId::Finish, StageCheckpoint::new(last_block))
                .expect("reset_engine_state: failed to save Finish stage checkpoint");
            provider_rw
                .remove_block_and_execution_above(keep_up_to)
                .expect("reset_engine_state: failed to remove blocks above reset point");
            provider_rw.commit().expect("reset_engine_state: failed to commit DB reset");
            guard.blockchain_provider = BlockchainProvider::new(guard.provider_factory.clone())
                .expect("reset_engine_state: failed to rebuild blockchain provider");
        }
    }

    /// Return a clone of the shared block-hash registry.
    pub fn block_hash_registry(&self) -> SharedBlockHashRegistry {
        self.block_registry.clone()
    }

    /// Return a clone of the shared L1 chain.
    ///
    /// Used by [`HarnessL1Server`](crate::HarnessL1Server) to serve
    /// `eth_getBlockByHash` / `eth_getBlockByNumber` to the production
    /// [`base_consensus_engine::BaseEngineClient`].
    pub fn l1_chain(&self) -> SharedL1Chain {
        self.l1_chain.clone()
    }

    /// Return the transactions committed for a given L2 block number.
    ///
    /// Returns `None` if the block has not been executed yet. The first
    /// transaction is always the L1 info deposit; subsequent transactions are
    /// user or upgrade-deposit transactions.
    pub fn block_transactions(&self, block_num: u64) -> Option<Vec<BaseTxEnvelope>> {
        self.inner
            .lock()
            .expect("action engine inner lock poisoned")
            .executed_transactions
            .get(&block_num)
            .cloned()
    }

    /// Read a storage value from the latest committed state.
    ///
    /// Accepts the slot as a `U256` for convenience (converted to `B256` internally).
    /// Returns `U256::ZERO` if the account or slot does not exist.
    pub fn storage_at(
        &self,
        address: Address,
        slot: alloy_primitives::U256,
    ) -> alloy_primitives::U256 {
        let slot_key: StorageKey = B256::from(slot);
        let inner = self.inner.lock().expect("engine client lock");
        let provider =
            inner.blockchain_provider.latest().expect("failed to get latest state provider");
        provider
            .storage(address, slot_key)
            .expect("failed to read storage")
            .map(alloy_primitives::U256::from)
            .unwrap_or(alloy_primitives::U256::ZERO)
    }

    /// Return the number of transactions executed in the given block.
    ///
    /// Returns `0` if no block with that number has been committed. A count of `1`
    /// means the block is deposit-only (L1 info deposit only, no user transactions).
    pub fn executed_tx_count(&self, block_number: u64) -> usize {
        self.inner
            .lock()
            .expect("engine client lock")
            .executed_transactions
            .get(&block_number)
            .map(|txs| txs.len())
            .unwrap_or(0)
    }

    /// Check whether an account has non-empty code deployed.
    ///
    /// Returns `true` if the account exists and has code, `false` otherwise.
    pub fn has_code(&self, address: Address) -> bool {
        let inner = self.inner.lock().expect("engine client lock");
        let provider =
            inner.blockchain_provider.latest().expect("failed to get latest state provider");
        provider
            .account_code(&address)
            .expect("failed to read account code")
            .is_some_and(|c: reth_primitives_traits::Bytecode| !c.is_empty())
    }

    /// Build a block from the given `BasePayloadAttributes` and commit it to the database,
    /// returning the `BaseBuiltPayload`.
    fn build_and_commit(
        inner: &mut ActionEngineClientInner,
        parent_hash: B256,
        attrs: BasePayloadAttributes,
    ) -> TransportResult<BaseBuiltPayload<BasePrimitives>> {
        // Look up the parent header from executed headers or fall back to the real genesis.
        // When building the first block the caller may pass B256::ZERO (the default rollup-config
        // genesis hash), but the Reth DB stores the genesis block under its actual computed hash.
        // Using the wrong hash here causes the payload builder to fail with "no state found".
        let (effective_parent_hash, parent_header) = inner
            .executed_headers
            .values()
            .find(|h| h.hash_slow() == parent_hash)
            .map(|h| (parent_hash, h.clone()))
            .unwrap_or_else(|| {
                // Genesis case: derive the real hash from the chain spec genesis header so the
                // builder can locate the committed state in the DB. Callers may pass either
                // B256::ZERO (rollup-config convention) or the actual computed genesis hash.
                let genesis_header = inner.chain_spec.genesis_header().clone();
                let genesis_hash = genesis_header.hash_slow();
                debug_assert!(
                    parent_hash == B256::ZERO || parent_hash == genesis_hash,
                    "unknown parent hash {parent_hash} — missed block or caller bug"
                );
                (genesis_hash, genesis_header)
            });

        let builder_attrs = BasePayloadBuilderAttributes::try_new(effective_parent_hash, attrs, 3)
            .map_err(|e| {
                TransportError::from(TransportErrorKind::custom_str(&format!(
                    "failed to create builder attributes: {e}"
                )))
            })?;

        let parent_sealed = SealedHeader::new(parent_header, effective_parent_hash);
        let config =
            PayloadConfig { parent_header: Arc::new(parent_sealed), attributes: builder_attrs };
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };

        let pool = TestPool::new();
        let payload_builder = BasePayloadBuilder::new(
            pool,
            inner.blockchain_provider.clone(),
            inner.evm_config.clone(),
        );
        let outcome = RethPayloadBuilder::try_build(&payload_builder, args).map_err(|e| {
            TransportError::from(TransportErrorKind::custom_str(&format!(
                "payload builder failed: {e}"
            )))
        })?;
        let built: BaseBuiltPayload<BasePrimitives> = outcome.into_payload().ok_or_else(|| {
            TransportError::from(TransportErrorKind::custom_str(
                "payload builder returned no payload",
            ))
        })?;

        // Commit the block state to the database so subsequent blocks can build on it.
        if let Some(executed) = built.executed_block() {
            let execution_output = executed.execution_output;
            let block_number = built.block().header().number();
            let execution_outcome = ExecutionOutcome {
                bundle: execution_output.state.clone(),
                receipts: vec![execution_output.result.receipts.clone()],
                first_block: block_number,
                requests: vec![execution_output.result.requests.clone()],
            };

            let state_provider = inner.provider_factory.provider().map_err(|e| {
                TransportError::from(TransportErrorKind::custom_str(&format!(
                    "failed to get provider: {e}"
                )))
            })?;
            let hashed_state = HashedPostStateProvider::hashed_post_state(
                &LatestStateProviderRef::new(&state_provider),
                &execution_output.state,
            );
            drop(state_provider);

            let provider_rw = inner.provider_factory.provider_rw().map_err(|e| {
                TransportError::from(TransportErrorKind::custom_str(&format!(
                    "failed to get provider_rw: {e}"
                )))
            })?;
            provider_rw
                .append_blocks_with_state(
                    vec![executed.recovered_block.as_ref().clone()],
                    &execution_outcome,
                    hashed_state.into_sorted(),
                )
                .map_err(|e| {
                    TransportError::from(TransportErrorKind::custom_str(&format!(
                        "failed to commit block state: {e}"
                    )))
                })?;
            provider_rw.commit().map_err(|e| {
                TransportError::from(TransportErrorKind::custom_str(&format!(
                    "failed to commit: {e}"
                )))
            })?;

            // Rebuild the blockchain provider so it sees the newly committed block.
            inner.blockchain_provider = BlockchainProvider::new(inner.provider_factory.clone())
                .map_err(|e| {
                    TransportError::from(TransportErrorKind::custom_str(&format!(
                        "failed to rebuild blockchain provider: {e}"
                    )))
                })?;
        }

        Ok(built)
    }

    /// Execute the transactions in a V1 payload against the production builder, returning the
    /// block hash.
    ///
    /// If this block was already executed during a `build_payload_inner` call (sequencer mode),
    /// execution is skipped and the pre-computed hash is returned directly.
    ///
    /// `parent_beacon_block_root` must be supplied for V3+ payloads (Ecotone and later)
    /// so that the reconstructed block header includes it and its `hash_slow()` matches
    /// what [`InsertTask`] computes from the same payload envelope.  Pass `None` for V1/V2
    /// payloads (pre-Ecotone).
    ///
    /// [`InsertTask`]: base_consensus_node::EngineActor
    fn execute_v1_inner(
        inner: &mut ActionEngineClientInner,
        registry: &SharedBlockHashRegistry,
        payload: &ExecutionPayloadV1,
        parent_beacon_block_root: Option<B256>,
    ) -> TransportResult<B256> {
        // Skip re-execution if this block was already built.
        //
        // In sequencer mode `payload.block_hash` is the real sealed hash; skip only when it
        // matches the stored header's hash to guard against hash collisions.
        //
        // In derivation mode the payload is constructed with a zeroed `block_hash`
        // placeholder because the engine is expected to fill it in. When we see
        // B256::ZERO we treat the block-number lookup alone as sufficient — the block was
        // pre-built by the sequencer and its state is already committed to the DB.
        if let Some(existing) = inner.executed_headers.get(&payload.block_number) {
            let existing_hash = existing.hash_slow();
            if payload.block_hash == B256::ZERO || payload.block_hash == existing_hash {
                return Ok(existing_hash);
            }
        }

        // Convert ExecutionPayloadV1 into BasePayloadAttributes for the builder.
        let attrs = BasePayloadAttributes {
            payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
                timestamp: payload.timestamp,
                prev_randao: payload.prev_randao,
                suggested_fee_recipient: payload.fee_recipient,
                withdrawals: Some(vec![]),
                parent_beacon_block_root,
            },
            transactions: Some(payload.transactions.clone()),
            no_tx_pool: Some(true),
            gas_limit: Some(payload.gas_limit),
            eip_1559_params: None,
            // Default to Some(0) so that Jovian blocks (which require a non-None
            // min_base_fee) can be re-executed when the skip-check does not fire.
            // The spec notes: "as long as MinBaseFee is not explicitly set, the
            // default value (0) will be systematically applied."
            min_base_fee: Some(0),
        };

        let built = Self::build_and_commit(inner, payload.parent_hash, attrs)?;
        let block = built.block();
        let hdr = block.header();
        let state_root = hdr.state_root();
        let block_hash = block.hash();

        if let Some(expected_root) = registry.get_state_root(payload.block_number) {
            assert_eq!(
                state_root, expected_root,
                "state root mismatch at block {}: computed={}, expected={}",
                payload.block_number, state_root, expected_root,
            );
        }

        // Register the state root in the block registry.
        registry.insert(payload.block_number, block_hash, Some(state_root));

        inner.executed_headers.insert(payload.block_number, hdr.clone());
        // Store the decoded transactions so `get_l2_block` can return full blocks.
        // `find_starting_forkchoice` calls `L2BlockInfo::from_block_and_genesis` on the unsafe
        // head, which reads the L1 info deposit from the first transaction.
        let txs: Vec<BaseTxEnvelope> = built.block().body().transactions.clone();
        inner.executed_transactions.insert(payload.block_number, txs);
        Ok(block_hash)
    }

    /// Execute and commit a block directly from full [`BasePayloadAttributes`], returning
    /// the resulting block hash.
    ///
    /// Unlike [`execute_v1_inner`], this method accepts the complete attributes including
    /// Holocene/Jovian-specific fields (`eip_1559_params`, `min_base_fee`), avoiding the
    /// lossy round-trip through [`ExecutionPayloadV1`] that strips those fields.
    ///
    /// If this block number was already executed (e.g. pre-built by the sequencer), the
    /// stored block hash is returned immediately without re-executing.
    ///
    /// [`execute_v1_inner`]: ActionEngineClient::execute_v1_inner
    pub fn execute_from_attrs(
        &self,
        parent_hash: B256,
        block_number: u64,
        attrs: BasePayloadAttributes,
    ) -> TransportResult<B256> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");

        // Skip re-execution if this block was already built.
        if let Some(existing) = guard.executed_headers.get(&block_number) {
            return Ok(existing.hash_slow());
        }

        let built = Self::build_and_commit(&mut guard, parent_hash, attrs)?;
        let block = built.block();
        let hdr = block.header();
        let state_root = hdr.state_root();
        let block_hash = block.hash();

        if let Some(expected_root) = self.block_registry.get_state_root(block_number) {
            assert_eq!(
                state_root, expected_root,
                "state root mismatch at block {block_number}: computed={state_root}, expected={expected_root}",
            );
        }

        self.block_registry.insert(block_number, block_hash, Some(state_root));
        guard.executed_headers.insert(block_number, hdr.clone());
        let txs: Vec<BaseTxEnvelope> = built.block().body().transactions.clone();
        guard.executed_transactions.insert(block_number, txs);
        Ok(block_hash)
    }

    /// Execute a fully-formed [`BaseBlock`] through the production engine, returning the block hash.
    ///
    /// Unlike [`execute_from_attrs`], this method extracts all necessary fields directly from the
    /// block (including `parent_beacon_block_root`), making it suitable for follow-node tests where
    /// only the completed block is available rather than the original attributes.
    ///
    /// If this block number was already executed (e.g. pre-built by a co-located sequencer), the
    /// stored block hash is returned immediately without re-executing.
    ///
    /// [`execute_from_attrs`]: ActionEngineClient::execute_from_attrs
    pub fn execute_block(&self, block: &BaseBlock) -> TransportResult<B256> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");

        let block_number = block.header.number;

        // Skip re-execution if already committed (e.g. pre-built by sequencer sharing this client).
        if let Some(existing) = guard.executed_headers.get(&block_number) {
            return Ok(existing.hash_slow());
        }

        // Encode all transactions to their EIP-2718 wire format.
        let txs: Vec<alloy_primitives::Bytes> = block
            .body
            .transactions
            .iter()
            .map(|tx| {
                let mut buf = Vec::new();
                tx.encode_2718(&mut buf);
                alloy_primitives::Bytes::from(buf)
            })
            .collect();

        let attrs = BasePayloadAttributes {
            payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
                timestamp: block.header.timestamp,
                prev_randao: block.header.mix_hash,
                suggested_fee_recipient: block.header.beneficiary,
                // Withdrawals are always empty on OP Stack — EIP-4895 is enabled but the
                // system never produces real withdrawals.
                withdrawals: Some(vec![]),
                parent_beacon_block_root: block.header.parent_beacon_block_root,
            },
            transactions: Some(txs),
            no_tx_pool: Some(true),
            gas_limit: Some(block.header.gas_limit),
            // eip_1559_params and min_base_fee are Holocene/Jovian fields. The default harness
            // config only activates up to Fjord, so None/Some(0) are correct defaults. Tests
            // that need post-Holocene blocks should extend this as needed.
            eip_1559_params: None,
            min_base_fee: Some(0),
        };

        let built = Self::build_and_commit(&mut guard, block.header.parent_hash, attrs)?;
        let built_block = built.block();
        let hdr = built_block.header();
        let state_root = hdr.state_root();
        let block_hash = built_block.hash();

        if let Some(expected_root) = self.block_registry.get_state_root(block_number) {
            assert_eq!(
                state_root, expected_root,
                "state root mismatch at block {block_number}: computed={state_root}, expected={expected_root}",
            );
        }

        self.block_registry.insert(block_number, block_hash, Some(state_root));
        guard.executed_headers.insert(block_number, hdr.clone());
        let txs: Vec<BaseTxEnvelope> = built_block.body().transactions.clone();
        guard.executed_transactions.insert(block_number, txs);
        Ok(block_hash)
    }

    /// Execute the transactions in `attrs`, build a pending payload, and return its `PayloadId`.
    ///
    /// Called from `fork_choice_updated_v2/v3` when `payload_attributes` is `Some`. The built
    /// payload is stored in `pending_payloads` for later retrieval via `get_payload_vX`.
    fn build_payload_inner(
        inner: &mut ActionEngineClientInner,
        registry: &SharedBlockHashRegistry,
        parent_hash: B256,
        attrs: &BasePayloadAttributes,
    ) -> TransportResult<PayloadId> {
        // Skip re-building if the block at this height was already committed (e.g. gossip arrived
        // before derivation). Reconstruct a PendingPayload from the stored header and transactions
        // so that the caller can proceed to get_payload_vX → new_payload_vX, where the
        // execute_v1_inner skip-check will fire and return the existing hash without re-executing.
        let parent_num = inner
            .executed_headers
            .values()
            .find(|h| h.hash_slow() == parent_hash)
            .map(|h| h.number)
            .or_else(|| {
                let genesis_hash = inner.chain_spec.genesis_header().hash_slow();
                if parent_hash == B256::ZERO || parent_hash == genesis_hash {
                    Some(inner.chain_spec.genesis_header().number)
                } else {
                    None
                }
            });
        if let Some(parent_num) = parent_num {
            let block_number = parent_num + 1;
            if let Some(existing_hdr) = inner.executed_headers.get(&block_number).cloned() {
                let existing_hash = existing_hdr.hash_slow();
                let txs =
                    inner.executed_transactions.get(&block_number).cloned().unwrap_or_default();
                let body = BaseBlockBody { transactions: txs, ommers: vec![], withdrawals: None };
                let sealed_block = SealedBlock::<BaseBlock>::from_parts_unchecked(
                    existing_hdr,
                    body,
                    existing_hash,
                );
                let id = PayloadId::new(inner.payload_counter.to_le_bytes());
                inner.payload_counter += 1;
                inner.pending_payloads.insert(
                    id,
                    PendingPayload {
                        built: BaseBuiltPayload::new(id, Arc::new(sealed_block), U256::ZERO, None),
                    },
                );
                return Ok(id);
            }
        }

        let built = Self::build_and_commit(inner, parent_hash, attrs.clone())?;

        let block = built.block();
        let hdr = block.header();
        let block_number = hdr.number();
        let state_root = hdr.state_root();
        let block_hash = block.hash();

        // Register the state root so derivation can validate against it.
        registry.insert(block_number, block_hash, Some(state_root));

        // Store the full header cloned from the built block so that `hash_slow()` on the stored
        // entry returns the actual block hash. Storing only a subset of fields would produce a
        // different hash and break the skip-check in `execute_v1_inner`.
        inner.executed_headers.insert(block_number, hdr.clone());
        let txs: Vec<BaseTxEnvelope> = block.body().transactions.clone();
        inner.executed_transactions.insert(block_number, txs);

        let id = PayloadId::new(inner.payload_counter.to_le_bytes());
        inner.payload_counter += 1;
        inner.pending_payloads.insert(id, PendingPayload { built });
        Ok(id)
    }

    const fn make_valid(block_hash: B256) -> PayloadStatus {
        PayloadStatus { status: PayloadStatusEnum::Valid, latest_valid_hash: Some(block_hash) }
    }

    const fn make_fcu_valid(head_block_hash: B256) -> ForkchoiceUpdated {
        ForkchoiceUpdated { payload_status: Self::make_valid(head_block_hash), payload_id: None }
    }

    fn header_to_l1_rpc_block(header: &Header, block_hash: B256) -> Block<EthTransaction> {
        let sealed = Sealed::new_unchecked(header.clone(), block_hash);
        let rpc_header = alloy_rpc_types_eth::Header::from_sealed(sealed);
        Block {
            header: rpc_header,
            uncles: vec![],
            transactions: BlockTransactions::Hashes(vec![]),
            withdrawals: None,
        }
    }

    fn header_to_l2_rpc_block(
        header: &Header,
        block_hash: B256,
        txs: Option<&Vec<BaseTxEnvelope>>,
    ) -> Block<BaseTransaction> {
        let sealed = Sealed::new_unchecked(header.clone(), block_hash);
        let rpc_header = alloy_rpc_types_eth::Header::from_sealed(sealed);
        let transactions = txs.map_or_else(
            || BlockTransactions::Hashes(vec![]),
            |txs| {
                // Wrap each consensus transaction in the RPC envelope required by
                // `L2BlockInfo::from_block_and_genesis` / `find_starting_forkchoice`.
                let full: Vec<BaseTransaction> = txs
                    .iter()
                    .enumerate()
                    .map(|(idx, tx)| {
                        let rpc_tx = EthTransaction {
                            inner: Recovered::new_unchecked(tx.clone(), Default::default()),
                            block_hash: Some(block_hash),
                            block_number: Some(header.number),
                            effective_gas_price: None,
                            transaction_index: Some(idx as u64),
                        };
                        BaseTransaction {
                            inner: rpc_tx,
                            deposit_nonce: None,
                            deposit_receipt_version: None,
                        }
                    })
                    .collect();
                BlockTransactions::Full(full)
            },
        );
        Block { header: rpc_header, uncles: vec![], transactions, withdrawals: None }
    }

    /// Look up an executed L2 block by number-or-tag from the in-process state.
    ///
    /// Used by [`HarnessEngineServer`](crate::HarnessEngineServer) to serve
    /// `eth_getBlockByNumber` requests over HTTP so the production
    /// [`base_consensus_engine::BaseEngineClient`] can bootstrap via
    /// `find_starting_forkchoice`.
    pub fn get_l2_block_by_numtag(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Option<Block<BaseTransaction>> {
        let guard = self.inner.lock().expect("action engine inner lock poisoned");
        match numtag {
            BlockNumberOrTag::Number(n) => guard.executed_headers.get(&n).map(|h| {
                let bh = h.hash_slow();
                let txs = guard.executed_transactions.get(&n);
                Self::header_to_l2_rpc_block(h, bh, txs)
            }),
            BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => {
                let number = guard.canonical_head.block_info.number;
                guard.executed_headers.get(&number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
            BlockNumberOrTag::Safe => {
                let number = guard.safe_head.block_info.number;
                guard.executed_headers.get(&number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
            BlockNumberOrTag::Finalized => {
                let number = guard.finalized_head.block_info.number;
                guard.executed_headers.get(&number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
            BlockNumberOrTag::Earliest => {
                let txs = guard.executed_transactions.get(&0);
                guard
                    .executed_headers
                    .get(&0)
                    .map(|h| Self::header_to_l2_rpc_block(h, h.hash_slow(), txs))
            }
        }
    }

    /// Look up an executed L2 block by hash from the in-process state.
    ///
    /// Used by [`HarnessEngineServer`](crate::HarnessEngineServer) to serve
    /// `eth_getBlockByHash` requests over HTTP.
    pub fn get_l2_block_by_hash(&self, hash: B256) -> Option<Block<BaseTransaction>> {
        let guard = self.inner.lock().expect("action engine inner lock poisoned");
        guard.executed_headers.values().find(|h| h.hash_slow() == hash).map(|h| {
            let bh = h.hash_slow();
            let txs = guard.executed_transactions.get(&h.number);
            Self::header_to_l2_rpc_block(h, bh, txs)
        })
    }

    /// Remove a pending payload by ID, returning a transport error if not found.
    fn take_pending(
        inner: &mut ActionEngineClientInner,
        payload_id: PayloadId,
    ) -> TransportResult<PendingPayload> {
        inner.pending_payloads.remove(&payload_id).ok_or_else(|| {
            TransportError::from(TransportErrorKind::custom_str(&format!(
                "ActionEngineClient: payload not found: {payload_id}"
            )))
        })
    }
}

#[async_trait]
impl EngineClient for ActionEngineClient {
    fn cfg(&self) -> &RollupConfig {
        &self.rollup_config
    }

    fn get_l1_block(&self, block: BlockId) -> EthGetBlock<<Ethereum as Network>::BlockResponse> {
        let chain = self.l1_chain.clone();
        let block_id = block;
        EthGetBlock::new_provider(
            block,
            Box::new(move |_kind| {
                let chain = chain.clone();
                ProviderCall::BoxedFuture(Box::pin(async move {
                    let rpc_block = match block_id {
                        BlockId::Number(num_or_tag) => {
                            let number = match num_or_tag {
                                BlockNumberOrTag::Number(n) => n,
                                _ => return Ok(None),
                            };
                            chain
                                .get_block(number)
                                .map(|l1| Self::header_to_l1_rpc_block(&l1.header, l1.hash()))
                        }
                        BlockId::Hash(block_hash) => chain
                            .block_by_hash(block_hash.block_hash)
                            .map(|l1| Self::header_to_l1_rpc_block(&l1.header, l1.hash())),
                    };
                    Ok(rpc_block)
                }))
            }),
        )
    }

    fn get_l2_block(&self, block: BlockId) -> EthGetBlock<<Base as Network>::BlockResponse> {
        let inner = Arc::clone(&self.inner);
        let block_id = block;
        EthGetBlock::new_provider(
            block,
            Box::new(move |_kind| {
                let inner = Arc::clone(&inner);
                ProviderCall::BoxedFuture(Box::pin(async move {
                    let guard = inner.lock().expect("action engine inner lock poisoned");
                    let rpc_block = match block_id {
                        BlockId::Number(num_or_tag) => {
                            let opt_number: Option<u64> = match num_or_tag {
                                BlockNumberOrTag::Number(n) => Some(n),
                                // Return the canonical (unsafe) head for Latest/Pending.
                                BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => {
                                    Some(guard.canonical_head.block_info.number)
                                }
                                // Safe and Finalized are not separately tracked: return None so
                                // `L2ForkchoiceState::current` falls back to genesis, which works
                                // without transactions via the `from_block_and_genesis` fast path.
                                BlockNumberOrTag::Safe | BlockNumberOrTag::Finalized => None,
                                BlockNumberOrTag::Earliest => Some(0),
                            };
                            opt_number.and_then(|number| {
                                guard.executed_headers.get(&number).map(|h| {
                                    let block_hash = h.hash_slow();
                                    let txs = guard.executed_transactions.get(&number);
                                    Self::header_to_l2_rpc_block(h, block_hash, txs)
                                })
                            })
                        }
                        BlockId::Hash(block_hash) => guard
                            .executed_headers
                            .values()
                            .find(|h| h.hash_slow() == block_hash.block_hash)
                            .map(|h| {
                                let bh = h.hash_slow();
                                let txs = guard.executed_transactions.get(&h.number);
                                Self::header_to_l2_rpc_block(h, bh, txs)
                            }),
                    };
                    Ok(rpc_block)
                }))
            }),
        )
    }

    fn get_proof(
        &self,
        address: Address,
        _keys: Vec<StorageKey>,
    ) -> RpcWithBlock<(Address, Vec<StorageKey>), EIP1186AccountProofResponse> {
        RpcWithBlock::new_provider(move |_block_id| {
            ProviderCall::BoxedFuture(Box::pin(async move {
                Ok(EIP1186AccountProofResponse {
                    address,
                    balance: Default::default(),
                    code_hash: Default::default(),
                    nonce: 0,
                    storage_hash: Default::default(),
                    account_proof: vec![],
                    storage_proof: vec![],
                })
            }))
        })
    }

    async fn l2_block_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<Block<BaseTransaction>>, EngineClientError> {
        let guard = self.inner.lock().expect("action engine inner lock poisoned");
        let block = match numtag {
            BlockNumberOrTag::Number(n) => guard.executed_headers.get(&n).map(|h| {
                let bh = h.hash_slow();
                let txs = guard.executed_transactions.get(&n);
                Self::header_to_l2_rpc_block(h, bh, txs)
            }),
            BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => {
                let number = guard.canonical_head.block_info.number;
                guard.executed_headers.get(&number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
            BlockNumberOrTag::Safe | BlockNumberOrTag::Finalized => {
                let number = guard.canonical_head.block_info.number;
                guard.executed_headers.get(&number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
            BlockNumberOrTag::Earliest => {
                guard.executed_headers.values().min_by_key(|h| h.number).map(|h| {
                    let bh = h.hash_slow();
                    let txs = guard.executed_transactions.get(&h.number);
                    Self::header_to_l2_rpc_block(h, bh, txs)
                })
            }
        };
        Ok(block)
    }

    async fn l2_block_info_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<L2BlockInfo>, EngineClientError> {
        let guard = self.inner.lock().expect("action engine inner lock poisoned");
        let info = match numtag {
            BlockNumberOrTag::Latest
            | BlockNumberOrTag::Safe
            | BlockNumberOrTag::Finalized
            | BlockNumberOrTag::Pending => Some(guard.canonical_head),
            BlockNumberOrTag::Number(n) => {
                if n == guard.canonical_head.block_info.number {
                    Some(guard.canonical_head)
                } else {
                    guard.executed_headers.get(&n).map(|h| {
                        let block_hash = h.hash_slow();
                        L2BlockInfo {
                            block_info: BlockInfo {
                                hash: block_hash,
                                number: h.number,
                                parent_hash: h.parent_hash,
                                timestamp: h.timestamp,
                            },
                            l1_origin: Default::default(),
                            seq_num: 0,
                        }
                    })
                }
            }
            BlockNumberOrTag::Earliest => None,
        };
        Ok(info)
    }
}

#[async_trait]
impl BaseEngineApi for ActionEngineClient {
    async fn new_payload_v2(
        &self,
        payload: ExecutionPayloadInputV2,
    ) -> TransportResult<PayloadStatus> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let block_hash = Self::execute_v1_inner(
            &mut guard,
            &self.block_registry,
            &payload.execution_payload,
            None,
        )?;
        Ok(Self::make_valid(block_hash))
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let block_hash = Self::execute_v1_inner(
            &mut guard,
            &self.block_registry,
            &payload.payload_inner.payload_inner,
            Some(parent_beacon_block_root),
        )?;
        Ok(Self::make_valid(block_hash))
    }

    async fn new_payload_v4(
        &self,
        payload: BaseExecutionPayloadV4,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let block_hash = Self::execute_v1_inner(
            &mut guard,
            &self.block_registry,
            &payload.payload_inner.payload_inner.payload_inner,
            Some(parent_beacon_block_root),
        )?;
        Ok(Self::make_valid(block_hash))
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<BasePayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        let head = fork_choice_state.head_block_hash;
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");

        // Update canonical head if the block is in our executed headers.
        if let Some(h) = guard.executed_headers.values().find(|h| h.hash_slow() == head).cloned() {
            let block_hash = head;
            guard.canonical_head = L2BlockInfo {
                block_info: BlockInfo {
                    hash: block_hash,
                    number: h.number,
                    parent_hash: h.parent_hash,
                    timestamp: h.timestamp,
                },
                l1_origin: Default::default(),
                seq_num: 0,
            };
        }

        // Update safe head when the FCU carries a non-zero safe hash.
        let safe = fork_choice_state.safe_block_hash;
        if safe != B256::ZERO
            && let Some(h) =
                guard.executed_headers.values().find(|h| h.hash_slow() == safe).cloned()
        {
            guard.safe_head = L2BlockInfo {
                block_info: BlockInfo {
                    hash: safe,
                    number: h.number,
                    parent_hash: h.parent_hash,
                    timestamp: h.timestamp,
                },
                l1_origin: Default::default(),
                seq_num: 0,
            };
        }

        // Update finalized head when the FCU carries a non-zero finalized hash.
        let fin = fork_choice_state.finalized_block_hash;
        if fin != B256::ZERO
            && let Some(h) = guard.executed_headers.values().find(|h| h.hash_slow() == fin).cloned()
        {
            guard.finalized_head = L2BlockInfo {
                block_info: BlockInfo {
                    hash: fin,
                    number: h.number,
                    parent_hash: h.parent_hash,
                    timestamp: h.timestamp,
                },
                l1_origin: Default::default(),
                seq_num: 0,
            };
        }

        // Sequencer mode: build a block from the provided attributes.
        if let Some(ref attrs) = payload_attributes {
            let payload_id =
                Self::build_payload_inner(&mut guard, &self.block_registry, head, attrs)?;
            return Ok(ForkchoiceUpdated {
                payload_status: Self::make_valid(head),
                payload_id: Some(payload_id),
            });
        }

        Ok(Self::make_fcu_valid(head))
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<BasePayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.fork_choice_updated_v2(fork_choice_state, payload_attributes).await
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<ExecutionPayloadEnvelopeV2> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let p = Self::take_pending(&mut guard, payload_id)?;
        Ok(ExecutionPayloadEnvelopeV2::from(p.built))
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<BaseExecutionPayloadEnvelopeV3> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let p = Self::take_pending(&mut guard, payload_id)?;
        Ok(BaseExecutionPayloadEnvelopeV3::from(p.built))
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<BaseExecutionPayloadEnvelopeV4> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let p = Self::take_pending(&mut guard, payload_id)?;
        Ok(BaseExecutionPayloadEnvelopeV4::from(p.built))
    }

    async fn get_payload_v5(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<BaseExecutionPayloadEnvelopeV5> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let p = Self::take_pending(&mut guard, payload_id)?;
        Ok(BaseExecutionPayloadEnvelopeV5::from(p.built))
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        _block_hashes: Vec<BlockHash>,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        Err(TransportError::from(TransportErrorKind::custom_str(
            "ActionEngineClient does not support get_payload_bodies_by_hash_v1",
        )))
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        _start: u64,
        _count: u64,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        Err(TransportError::from(TransportErrorKind::custom_str(
            "ActionEngineClient does not support get_payload_bodies_by_range_v1",
        )))
    }

    async fn get_client_version_v1(
        &self,
        _client_version: ClientVersionV1,
    ) -> TransportResult<Vec<ClientVersionV1>> {
        Err(TransportError::from(TransportErrorKind::custom_str(
            "ActionEngineClient does not support get_client_version_v1",
        )))
    }

    async fn exchange_capabilities(
        &self,
        _capabilities: Vec<String>,
    ) -> TransportResult<Vec<String>> {
        Err(TransportError::from(TransportErrorKind::custom_str(
            "ActionEngineClient does not support exchange_capabilities",
        )))
    }
}

#[async_trait]
impl SequencerEngineClient for ActionEngineClient {
    async fn reset_engine_forkchoice(&self) -> Result<(), NodeEngineClientError> {
        // No-op in action tests — FCU to current head is the default.
        Ok(())
    }

    async fn start_build_block(
        &self,
        attributes: AttributesWithParent,
    ) -> Result<PayloadId, NodeEngineClientError> {
        let parent_hash = attributes.parent.block_info.hash;
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        Self::build_payload_inner(
            &mut guard,
            &self.block_registry,
            parent_hash,
            &attributes.attributes,
        )
        .map_err(|e| NodeEngineClientError::RequestError(e.to_string()))
    }

    async fn get_sealed_payload(
        &self,
        payload_id: PayloadId,
        _attributes: AttributesWithParent,
    ) -> Result<BaseExecutionPayloadEnvelope, NodeEngineClientError> {
        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        let pending = Self::take_pending(&mut guard, payload_id)
            .map_err(|e| NodeEngineClientError::ResponseError(e.to_string()))?;
        let block = pending.built.block();
        let block_hash = block.hash();
        let parent_beacon_block_root = block.header().parent_beacon_block_root();
        let (payload, _sidecar) =
            BaseExecutionPayload::from_block_unchecked(block_hash, &block.clone_block());
        Ok(BaseExecutionPayloadEnvelope { parent_beacon_block_root, execution_payload: payload })
    }

    async fn insert_unsafe_payload(
        &self,
        payload: BaseExecutionPayloadEnvelope,
    ) -> Result<(), NodeEngineClientError> {
        // Extract the V1 payload and parent_beacon_block_root for execution.
        let pbbr = payload.parent_beacon_block_root;
        let v1 = payload.execution_payload.as_v1();
        let head_hash = v1.block_hash;

        let mut guard = self.inner.lock().expect("action engine inner lock poisoned");
        Self::execute_v1_inner(&mut guard, &self.block_registry, v1, pbbr)
            .map_err(|e| NodeEngineClientError::RequestError(e.to_string()))?;

        // Update canonical head.
        if let Some(h) =
            guard.executed_headers.values().find(|h| h.hash_slow() == head_hash).cloned()
        {
            guard.canonical_head = L2BlockInfo {
                block_info: BlockInfo {
                    hash: head_hash,
                    number: h.number,
                    parent_hash: h.parent_hash,
                    timestamp: h.timestamp,
                },
                l1_origin: Default::default(),
                seq_num: 0,
            };
        }
        Ok(())
    }

    async fn get_unsafe_head(&self) -> Result<L2BlockInfo, NodeEngineClientError> {
        let guard = self.inner.lock().expect("action engine inner lock poisoned");
        Ok(guard.canonical_head)
    }
}
