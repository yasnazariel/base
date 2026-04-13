#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod sync_target;
use std::{sync::Arc, time::Duration};

use alloy_consensus::BlockHeader;
use alloy_eips::eip1898::BlockWithParent;
use base_execution_trie::{
    BaseProofStoragePrunerTask, OpProofsStorage, OpProofsStore, live::LiveTrieCollector,
    metrics::BlockMetrics,
};
use futures::TryStreamExt;
use reth_execution_types::Chain;
use reth_exex::{ExExContext, ExExEvent, ExExNotification, ExExNotificationsStream};
use reth_node_api::{FullNodeComponents, NodePrimitives, NodeTypes};
use reth_provider::{BlockNumReader, BlockReader, TransactionVariant};
pub use sync_target::{CachedBlockTrieData, SyncTarget, SyncTargetState};
use tokio::task;
use tracing::{debug, error, info};

// Safety threshold for maximum blocks to prune automatically on startup.
// If the required prune exceeds this, the node will error out and require manual pruning. Default
// is 1000 blocks.
const MAX_PRUNE_BLOCKS_STARTUP: u64 = 1000;

/// How many blocks to process in a single batch before yielding. Default is 50 blocks.
const SYNC_BLOCKS_BATCH_SIZE: usize = 50;

/// Default proofs history window: 1 month of blocks at 2s block time
const DEFAULT_PROOFS_HISTORY_WINDOW: u64 = 1_296_000;

/// Default interval between proof-storage prune runs. Default is 15 seconds.
const DEFAULT_PRUNE_INTERVAL: Duration = Duration::from_secs(15);

/// Default verification interval: disabled
const DEFAULT_VERIFICATION_INTERVAL: u64 = 0; // disabled

/// Builder for [`OpProofsExEx`].
#[derive(Debug)]
pub struct BaseProofsExExBuilder<Node, Storage>
where
    Node: FullNodeComponents,
{
    ctx: ExExContext<Node>,
    storage: OpProofsStorage<Storage>,
    proofs_history_window: u64,
    proofs_history_prune_interval: Duration,
    verification_interval: u64,
}

impl<Node, Storage> BaseProofsExExBuilder<Node, Storage>
where
    Node: FullNodeComponents,
{
    /// Create a new builder with required parameters and defaults.
    pub const fn new(ctx: ExExContext<Node>, storage: OpProofsStorage<Storage>) -> Self {
        Self {
            ctx,
            storage,
            proofs_history_window: DEFAULT_PROOFS_HISTORY_WINDOW,
            proofs_history_prune_interval: DEFAULT_PRUNE_INTERVAL,
            verification_interval: DEFAULT_VERIFICATION_INTERVAL,
        }
    }

    /// Sets the window to span blocks for proofs history.
    pub const fn with_proofs_history_window(mut self, window: u64) -> Self {
        self.proofs_history_window = window;
        self
    }

    /// Sets the interval between proof-storage prune runs.
    pub const fn with_proofs_history_prune_interval(mut self, interval: Duration) -> Self {
        self.proofs_history_prune_interval = interval;
        self
    }

    /// Sets the verification interval.
    pub const fn with_verification_interval(mut self, interval: u64) -> Self {
        self.verification_interval = interval;
        self
    }

    /// Builds the [`OpProofsExEx`].
    pub fn build(self) -> OpProofsExEx<Node, Storage> {
        OpProofsExEx {
            ctx: self.ctx,
            storage: self.storage,
            proofs_history_window: self.proofs_history_window,
            proofs_history_prune_interval: self.proofs_history_prune_interval,
            verification_interval: self.verification_interval,
        }
    }
}

/// OP Proofs `ExEx` - processes blocks and tracks state changes within fault proof window.
///
/// Saves and serves trie nodes to make proofs faster. This handles the process of
/// saving the current state, new blocks as they're added, and serving proof RPCs
/// based on the saved data.
///
/// # Examples
///
/// The following example shows how to install the `ExEx` with either in-memory or persistent storage.
/// This can be used when launching an OP-Reth node via a binary.
/// We are currently using it in optimism/bin/src/main.rs.
///
/// ```
/// use futures::FutureExt;
/// use reth_db::test_utils::create_test_rw_db;
/// use reth_node_api::NodeTypesWithDBAdapter;
/// use reth_node_builder::{NodeBuilder, NodeConfig};
/// use base_execution_chainspec::BASE_MAINNET;
/// use base_execution_exex::OpProofsExEx;
/// use base_node_core::{OpNode, args::RollupArgs};
/// use base_execution_trie::{InMemoryProofsStorage, OpProofsStorage, db::MdbxProofsStorage};
/// use reth_provider::providers::BlockchainProvider;
/// use std::{sync::Arc, time::Duration};
///
/// let config = NodeConfig::new(BASE_MAINNET.clone());
/// let db = create_test_rw_db();
/// let args = RollupArgs::default();
/// let op_node = OpNode::new(args);
///
/// // Create in-memory or persistent storage
/// let storage: OpProofsStorage<Arc<InMemoryProofsStorage>> =
///     Arc::new(InMemoryProofsStorage::new()).into();
///
/// // Example for creating persistent storage
/// # let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
/// # let storage_path = temp_dir.path().join("proofs_storage");
///
/// # let storage: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::new(
/// #    MdbxProofsStorage::new(&storage_path).expect("Failed to create MdbxProofsStorage"),
/// # ).into();
///
/// let storage_exec = storage.clone();
/// let proofs_history_window = 1_296_000u64;
/// let proofs_history_prune_interval = Duration::from_secs(3600);
///
/// // Verification interval: perform full execution every N blocks
/// let verification_interval = 0; // 0 = disabled, 100 = verify every 100 blocks
///
/// // Can also use install_exex_if along with a boolean flag
/// // Set this based on your configuration or CLI args
/// let _builder = NodeBuilder::new(config)
///     .with_database(db)
///     .with_types_and_provider::<OpNode, BlockchainProvider<NodeTypesWithDBAdapter<OpNode, _>>>()
///     .with_components(op_node.components())
///     .install_exex("proofs-history", move |exex_context| async move {
///         Ok(OpProofsExEx::builder(exex_context, storage_exec)
///             .with_proofs_history_window(proofs_history_window)
///             .with_proofs_history_prune_interval(proofs_history_prune_interval)
///             .with_verification_interval(verification_interval)
///             .build()
///             .run()
///             .boxed())
///     })
///     .on_node_started(|_full_node| Ok(()))
///     .check_launch();
/// ```
#[derive(Debug)]
pub struct OpProofsExEx<Node, Storage>
where
    Node: FullNodeComponents,
{
    /// The `ExEx` context containing the node related utilities e.g. provider, notifications,
    /// events.
    ctx: ExExContext<Node>,
    /// The type of storage DB.
    storage: OpProofsStorage<Storage>,
    /// The window to span blocks for proofs history. Value is the number of blocks, received as
    /// cli arg.
    proofs_history_window: u64,
    /// Interval between proof-storage prune runs
    proofs_history_prune_interval: Duration,
    /// Verification interval: perform full block execution every N blocks for data integrity.
    /// If 0, verification is disabled (always use fast path when available).
    /// If 1, verification is always enabled (always execute blocks).
    verification_interval: u64,
}

impl<Node, Storage> OpProofsExEx<Node, Storage>
where
    Node: FullNodeComponents,
{
    /// Create a new `OpProofsExEx` instance.
    pub fn new(ctx: ExExContext<Node>, storage: OpProofsStorage<Storage>) -> Self {
        BaseProofsExExBuilder::new(ctx, storage).build()
    }

    /// Create a new builder for `OpProofsExEx`.
    pub const fn builder(
        ctx: ExExContext<Node>,
        storage: OpProofsStorage<Storage>,
    ) -> BaseProofsExExBuilder<Node, Storage> {
        BaseProofsExExBuilder::new(ctx, storage)
    }
}

impl<Node, Storage, Primitives> OpProofsExEx<Node, Storage>
where
    Node: FullNodeComponents<Types: NodeTypes<Primitives = Primitives>>,
    Primitives: NodePrimitives,
    Storage: OpProofsStore + Clone + 'static,
{
    /// Main execution loop for the `ExEx`
    pub async fn run(mut self) -> eyre::Result<()> {
        self.ensure_initialized()?;
        let sync_target = self.spawn_sync_task();

        // If storage is behind tip, start syncing immediately rather than waiting
        // for the first notification.
        let best_block = self.ctx.provider().best_block_number()?;
        let latest_stored = self.storage.get_latest_block_number()?.map(|(n, _)| n).unwrap_or(0);
        if latest_stored < best_block {
            info!(
                target: "base::exex",
                latest_stored,
                best_block,
                "Storage behind tip, starting sync immediately"
            );
            sync_target.update_state(SyncTargetState::SyncUpTo { to: best_block });
        }

        let prune_task = BaseProofStoragePrunerTask::new(
            self.storage.clone(),
            self.ctx.provider().clone(),
            self.proofs_history_window,
            self.proofs_history_prune_interval,
        );
        self.ctx
            .task_executor()
            .spawn_with_graceful_shutdown_signal(|signal| Box::pin(prune_task.run(signal)));

        self.ctx.notifications.set_without_head();

        while let Some(notification) = self.ctx.notifications.try_next().await? {
            self.handle_notification(notification, &sync_target)?;
        }

        Ok(())
    }

    /// Ensure proofs storage is initialized
    fn ensure_initialized(&self) -> eyre::Result<()> {
        // Check if proofs storage is initialized
        let earliest_block_number = match self.storage.get_earliest_block_number()? {
            Some((n, _)) => n,
            None => {
                return Err(eyre::eyre!(
                    "Proofs storage not initialized. Please run 'op-reth initialize-op-proofs --proofs-history.storage-path <PATH>' first."
                ));
            }
        };

        let latest_block_number = match self.storage.get_latest_block_number()? {
            Some((n, _)) => n,
            None => {
                return Err(eyre::eyre!(
                    "Proofs storage not initialized. Please run 'op-reth initialize-op-proofs --proofs-history.storage-path <PATH>' first."
                ));
            }
        };

        // Check if we have accumulated too much history for the configured window.
        // If the gap between what we have and what we want to keep is too large, the auto-pruner
        // will stall the node.
        let target_earliest = latest_block_number.saturating_sub(self.proofs_history_window);
        if target_earliest > earliest_block_number {
            let blocks_to_prune = target_earliest - earliest_block_number;
            if blocks_to_prune > MAX_PRUNE_BLOCKS_STARTUP {
                return Err(eyre::eyre!(
                    "Configuration requires pruning {} blocks, which exceeds the safety threshold of {}. \
                     Huge prune operations can stall the node. \
                     Please run 'op-reth proofs prune' manually before starting the node.",
                    blocks_to_prune,
                    MAX_PRUNE_BLOCKS_STARTUP
                ));
            }
        }

        // Need to update the earliest block metric on startup as this is not called frequently and
        // can show outdated info. When metrics are disabled, this is a no-op.
        BlockMetrics::earliest_number().set(earliest_block_number as f64);

        Ok(())
    }

    /// Spawn the background sync task and return the shared [`SyncTarget`].
    ///
    /// The sync target buffers trie data from notifications so the sync loop
    /// can use pre-computed data for blocks even when it is many blocks behind
    /// the chain tip. Blocks whose trie data was evicted from the cache fall
    /// back to full execution.
    fn spawn_sync_task(&self) -> Arc<SyncTarget> {
        let sync_target = Arc::new(SyncTarget::new());
        let task_sync_target = Arc::clone(&sync_target);

        let task_storage = self.storage.clone();
        let task_provider = self.ctx.provider().clone();
        let task_evm_config = self.ctx.evm_config().clone();
        let verification_interval = self.verification_interval;

        self.ctx.task_executor().spawn_critical_task(
            "base::exex::proofs_storage_sync_loop",
            async move {
                let storage = task_storage.clone();
                let task_collector =
                    LiveTrieCollector::new(task_evm_config, task_provider.clone(), &storage);
                Self::sync_loop(
                    task_sync_target,
                    task_storage,
                    task_provider,
                    &task_collector,
                    verification_interval,
                )
                .await;
            },
        );

        sync_target
    }

    async fn sync_loop(
        sync_target: Arc<SyncTarget>,
        storage: OpProofsStorage<Storage>,
        provider: Node::Provider,
        collector: &LiveTrieCollector<'_, Node::Evm, Node::Provider, Storage>,
        verification_interval: u64,
    ) {
        info!(target: "base::exex", "Starting proofs storage sync loop");

        loop {
            let Some(state) = sync_target.take_state() else {
                sync_target.notified().await;
                continue;
            };

            match state {
                SyncTargetState::Revert { revert_to } => {
                    Self::handle_revert(&storage, collector, revert_to);
                    sync_target.mark_revert_complete(&revert_to);
                }
                SyncTargetState::RevertThenSync { revert_to, sync_to } => {
                    Self::handle_revert(&storage, collector, revert_to);
                    sync_target.mark_revert_complete(&revert_to);
                    Self::sync_forward(
                        &sync_target,
                        &storage,
                        &provider,
                        collector,
                        verification_interval,
                        sync_to,
                    )
                    .await;
                }
                SyncTargetState::SyncUpTo { to } => {
                    Self::sync_forward(
                        &sync_target,
                        &storage,
                        &provider,
                        collector,
                        verification_interval,
                        to,
                    )
                    .await;
                }
            }
        }
    }

    fn handle_revert(
        storage: &OpProofsStorage<Storage>,
        collector: &LiveTrieCollector<'_, Node::Evm, Node::Provider, Storage>,
        revert_to: BlockWithParent,
    ) {
        let latest = match storage.get_latest_block_number() {
            Ok(Some((n, _))) => n,
            Ok(None) => return,
            Err(e) => {
                error!(target: "base::exex", error = ?e, "Failed to get latest block during revert");
                return;
            }
        };

        if latest >= revert_to.block.number {
            info!(
                target: "base::exex",
                revert_to = revert_to.block.number,
                latest,
                "Reverting proofs storage"
            );
            if let Err(e) = collector.unwind_history(revert_to) {
                error!(target: "base::exex", error = ?e, "Failed to revert proofs storage");
            }
        } else {
            debug!(
                target: "base::exex",
                revert_to = revert_to.block.number,
                latest,
                "Revert target beyond stored blocks, skipping"
            );
        }
    }

    async fn sync_forward(
        sync_target: &SyncTarget,
        storage: &OpProofsStorage<Storage>,
        provider: &Node::Provider,
        collector: &LiveTrieCollector<'_, Node::Evm, Node::Provider, Storage>,
        verification_interval: u64,
        target: u64,
    ) {
        loop {
            // Check for higher-priority state (e.g. revert) before processing.
            if sync_target.has_pending_state() {
                return;
            }

            let latest = match storage.get_latest_block_number() {
                Ok(Some((n, _))) => n,
                Ok(None) => {
                    error!(target: "base::exex", "No blocks stored in proofs storage during sync");
                    return;
                }
                Err(e) => {
                    error!(target: "base::exex", error = ?e, "Failed to get latest block");
                    return;
                }
            };

            if latest >= target {
                return;
            }

            let end = (latest + SYNC_BLOCKS_BATCH_SIZE as u64).min(target);
            let cache_len = sync_target.cache_len();
            info!(
                target: "base::exex",
                start = latest + 1,
                end,
                target,
                batch_size = end - latest,
                cache_len,
                "Processing proofs storage sync batch"
            );

            for block_num in (latest + 1)..=end {
                let cached = sync_target.take(block_num);
                if let Err(e) = Self::process_block(
                    block_num,
                    cached,
                    collector,
                    provider,
                    verification_interval,
                ) {
                    error!(target: "base::exex", block_number = block_num, error = ?e, "Block processing failed");
                    return;
                }
            }

            info!(target: "base::exex", latest_stored = latest, target, "Batch processed, yielding");
            task::yield_now().await;
        }
    }

    fn process_block(
        block_number: u64,
        cached: Option<CachedBlockTrieData>,
        collector: &LiveTrieCollector<'_, Node::Evm, Node::Provider, Storage>,
        provider: &Node::Provider,
        verification_interval: u64,
    ) -> eyre::Result<()> {
        let should_verify =
            verification_interval > 0 && block_number.is_multiple_of(verification_interval);

        if let Some(cached) = cached {
            if !should_verify {
                debug!(
                    target: "base::exex",
                    block_number,
                    "Using pre-computed state from notification"
                );

                collector.store_block_updates(
                    cached.block_with_parent,
                    (*cached.trie_updates).clone(),
                    (*cached.hashed_state).clone(),
                )?;

                return Ok(());
            }

            info!(
                target: "base::exex",
                block_number,
                verification_interval,
                "Periodic verification: performing full block execution despite cached data"
            );
        } else {
            debug!(
                target: "base::exex",
                block_number,
                "No cached trie data, falling back to full execution"
            );
        }

        debug!(
            target: "base::exex",
            block_number,
            "Fetching block from provider for execution",
        );

        let block = provider
            .recovered_block(block_number.into(), TransactionVariant::NoHash)?
            .ok_or_else(|| eyre::eyre!("Missing block {} in provider", block_number))?;

        collector.execute_and_store_block_updates(&block)?;
        Ok(())
    }

    fn handle_notification(
        &self,
        notification: ExExNotification<Primitives>,
        sync_target: &SyncTarget,
    ) -> eyre::Result<()> {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                self.handle_chain_committed(Arc::clone(new), sync_target)?
            }
            ExExNotification::ChainReorged { old, new } => {
                self.handle_chain_reorged(Arc::clone(old), Arc::clone(new), sync_target)?
            }
            ExExNotification::ChainReverted { old } => {
                self.handle_chain_reverted(Arc::clone(old), sync_target)?
            }
        }

        if let Some(committed_chain) = notification.committed_chain() {
            let tip = committed_chain.tip().num_hash();
            debug!(
                target: "base::exex",
                block_number = tip.number,
                block_hash = ?tip.hash,
                "Sending FinishedHeight event"
            );
            self.ctx.events.send(ExExEvent::FinishedHeight(tip))?;
        }

        Ok(())
    }

    fn handle_chain_committed(
        &self,
        new: Arc<Chain<Primitives>>,
        sync_target: &SyncTarget,
    ) -> eyre::Result<()> {
        debug!(
            target: "base::exex",
            block_number = new.tip().number(),
            block_hash = ?new.tip().hash(),
            "ChainCommitted notification received",
        );

        // Cache trie data for all blocks in the chain so the sync loop can
        // use pre-computed data even when it is many blocks behind.
        let total_blocks = new.blocks().len();
        let mut cached_count = 0usize;
        for (&block_number, block) in new.blocks() {
            if let Some(trie_data) = new.trie_data_at(block_number) {
                let sorted = trie_data.get();
                sync_target.insert(
                    block_number,
                    CachedBlockTrieData {
                        block_with_parent: block.block_with_parent(),
                        hashed_state: Arc::clone(&sorted.hashed_state),
                        trie_updates: Arc::clone(&sorted.trie_updates),
                    },
                );
                cached_count += 1;
            } else {
                debug!(
                    target: "base::exex",
                    block_number,
                    "Notification block missing trie data"
                );
            }
        }

        let cache_len = sync_target.cache_len();
        info!(
            target: "base::exex",
            tip = new.tip().number(),
            total_blocks,
            cached_count,
            missing = total_blocks - cached_count,
            cache_len,
            "Cached notification trie data"
        );

        sync_target.update_state(SyncTargetState::SyncUpTo { to: new.tip().number() });
        Ok(())
    }

    fn handle_chain_reorged(
        &self,
        old: Arc<Chain<Primitives>>,
        new: Arc<Chain<Primitives>>,
        sync_target: &SyncTarget,
    ) -> eyre::Result<()> {
        info!(
            target: "base::exex",
            old_block_number = old.tip().number(),
            old_block_hash = ?old.tip().hash(),
            new_block_number = new.tip().number(),
            new_block_hash = ?new.tip().hash(),
            "ChainReorged notification received",
        );

        if old.fork_block() != new.fork_block() {
            return Err(eyre::eyre!(
                "Fork blocks do not match: old fork block {:?}, new fork block {:?}",
                old.fork_block(),
                new.fork_block()
            ));
        }

        let first_old = old.first().block_with_parent();

        // Invalidate any cached blocks from the old chain.
        sync_target.clear_from(first_old.block.number);

        // Cache trie data for all blocks in the new chain.
        for (&block_number, block) in new.blocks() {
            if let Some(trie_data) = new.trie_data_at(block_number) {
                let sorted = trie_data.get();
                sync_target.insert(
                    block_number,
                    CachedBlockTrieData {
                        block_with_parent: block.block_with_parent(),
                        hashed_state: Arc::clone(&sorted.hashed_state),
                        trie_updates: Arc::clone(&sorted.trie_updates),
                    },
                );
            } else {
                debug!(
                    target: "base::exex",
                    block_number,
                    "Reorged block missing trie data"
                );
            }
        }

        sync_target.update_state(SyncTargetState::RevertThenSync {
            revert_to: first_old,
            sync_to: new.tip().number(),
        });

        Ok(())
    }

    fn handle_chain_reverted(
        &self,
        old: Arc<Chain<Primitives>>,
        sync_target: &SyncTarget,
    ) -> eyre::Result<()> {
        info!(
            target: "base::exex",
            old_block_number = old.tip().number(),
            old_block_hash = ?old.tip().hash(),
            "ChainReverted notification received",
        );

        let first_old = old.first().block_with_parent();

        // Invalidate any cached blocks that are being reverted.
        sync_target.clear_from(first_old.block.number);

        sync_target.update_state(SyncTargetState::Revert { revert_to: first_old });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, default::Default, sync::Arc, time::Duration};

    use alloy_consensus::private::alloy_primitives::B256;
    use alloy_eips::{BlockNumHash, NumHash, eip1898::BlockWithParent};
    use base_execution_trie::{
        BlockStateDiff, OpProofsStorage, OpProofsStore, db::MdbxProofsStorage,
    };
    use reth_db::test_utils::tempdir_path;
    use reth_ethereum_primitives::{Block, Receipt};
    use reth_execution_types::{Chain, ExecutionOutcome};
    use reth_primitives_traits::RecoveredBlock;
    use reth_trie::{HashedPostStateSorted, LazyTrieData, updates::TrieUpdatesSorted};

    use super::*;

    // -------------------------------------------------------------------------
    // Helpers: deterministic blocks and deterministic Chain with precomputed updates
    // -------------------------------------------------------------------------
    fn b256(byte: u8) -> B256 {
        B256::new([byte; 32])
    }

    // deterministic hash from block number: 0 -> 0x00.., 1 -> 0x01.., etc.
    fn hash_for_num(num: u64) -> B256 {
        // if you only care about small test numbers, this is enough:
        b256(num as u8)

        // If you want to avoid wrapping when num > 255, use something like:
        // let mut out = [0u8; 32];
        // out[0..8].copy_from_slice(&num.to_be_bytes());
        // B256::new(out)
    }

    fn mk_block(num: u64) -> RecoveredBlock<Block> {
        let mut b: RecoveredBlock<Block> = Default::default();
        b.set_block_number(num);
        b.set_hash(hash_for_num(num));
        b.set_parent_hash(hash_for_num(num - 1));
        b
    }

    fn mk_chain_with_updates(
        from: u64,
        to: u64,
        hash_override: Option<B256>,
    ) -> Chain<reth_ethereum_primitives::EthPrimitives> {
        let mut blocks: Vec<RecoveredBlock<Block>> = Vec::new();
        let mut trie_data = BTreeMap::new();

        for n in from..=to {
            let mut b = mk_block(n);
            if let Some(hash) = hash_override {
                b.set_hash(hash);
            }
            blocks.push(b);

            let data = LazyTrieData::ready(
                Arc::new(HashedPostStateSorted::default()),
                Arc::new(TrieUpdatesSorted::default()),
            );
            trie_data.insert(n, data);
        }

        let execution_outcome: ExecutionOutcome<Receipt> = ExecutionOutcome {
            bundle: Default::default(),
            receipts: Vec::new(),
            requests: Vec::new(),
            first_block: from,
        };

        Chain::new(blocks, execution_outcome, trie_data)
    }

    /// Store blocks directly into proofs storage (bypasses the sync loop).
    fn store_blocks<S: OpProofsStore>(from: u64, to: u64, storage: &OpProofsStorage<S>) {
        for n in from..=to {
            let chain = mk_chain_with_updates(n, n, None);
            let block = chain.blocks().get(&n).unwrap();
            storage
                .store_trie_updates(block.block_with_parent(), BlockStateDiff::default())
                .expect("store trie update");
        }
    }

    fn init_storage<S: OpProofsStore>(storage: OpProofsStorage<S>) {
        let genesis_block = NumHash::new(0, b256(0x00));
        storage
            .set_earliest_block_number(genesis_block.number, genesis_block.hash)
            .expect("set earliest");
        storage
            .store_trie_updates(
                BlockWithParent::new(genesis_block.hash, genesis_block),
                BlockStateDiff::default(),
            )
            .expect("store trie update");
    }

    // Initialize exex with config
    fn build_test_exex<NodeT, Store>(
        ctx: ExExContext<NodeT>,
        storage: OpProofsStorage<Store>,
    ) -> OpProofsExEx<NodeT, Store>
    where
        NodeT: FullNodeComponents,
        Store: OpProofsStore + Clone + 'static,
    {
        OpProofsExEx::builder(ctx, storage)
            .with_proofs_history_window(20)
            .with_proofs_history_prune_interval(Duration::from_secs(3600))
            .with_verification_interval(1000)
            .build()
    }

    #[tokio::test]
    async fn handle_notification_chain_committed() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let new_chain = Arc::new(mk_chain_with_updates(1, 1, None));
        let notif = ExExNotification::ChainCommitted { new: new_chain };

        let sync_target = SyncTarget::new();

        exex.handle_notification(notif, &sync_target).expect("handle chain commit");

        // Committed blocks are cached in sync target, not stored directly
        assert!(sync_target.take(1).is_some(), "block 1 should be cached");
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(state, SyncTargetState::SyncUpTo { to: 1 }));
    }

    #[tokio::test]
    async fn handle_notification_chain_committed_caches_already_stored_blocks() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());

        // Pre-store blocks 1..5 so storage is at block 5
        store_blocks(1, 5, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let sync_target = SyncTarget::new();

        // Handle notification for block 5 which is already stored - still caches
        let new_chain = Arc::new(mk_chain_with_updates(5, 5, Some(hash_for_num(10))));
        let notif = ExExNotification::ChainCommitted { new: new_chain };
        exex.handle_notification(notif, &sync_target).expect("handle chain commit");

        // State is set (sync loop will see latest >= target and skip)
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(state, SyncTargetState::SyncUpTo { to: 5 }));

        // Storage is unchanged (notification handler doesn't write to storage)
        let latest = proofs.get_latest_block_number().expect("get latest block").expect("ok");
        assert_eq!(latest.0, 5);
        assert_eq!(latest.1, hash_for_num(5));
    }

    #[tokio::test]
    async fn handle_notification_chain_reorged() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());
        store_blocks(1, 10, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let sync_target = SyncTarget::new();

        // Now the tip is 10, and we want to reorg from block 6..12
        let old_chain = Arc::new(mk_chain_with_updates(6, 10, None));
        let new_chain = Arc::new(mk_chain_with_updates(6, 12, None));

        // Notification: chain reorged 6..12
        let notif = ExExNotification::ChainReorged { new: new_chain, old: old_chain };

        exex.handle_notification(notif, &sync_target).expect("handle chain re-orged");

        // Should have RevertThenSync state
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(
            state,
            SyncTargetState::RevertThenSync { revert_to, sync_to: 12 }
            if revert_to.block.number == 6
        ));

        // New chain blocks should be cached
        for n in 6..=12 {
            assert!(sync_target.take(n).is_some(), "block {n} should be cached");
        }

        // Storage unchanged (sync loop handles the actual revert)
        let latest = proofs.get_latest_block_number().expect("get latest block").expect("ok").0;
        assert_eq!(latest, 10);
    }

    #[tokio::test]
    async fn handle_notification_chain_reorged_beyond_stored_blocks() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());
        store_blocks(1, 10, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let sync_target = SyncTarget::new();

        // Now the tip is 10, and we want to reorg from block 12..15
        // Both chains share the same fork block (block 11)
        let old_chain = Arc::new(mk_chain_with_updates(12, 15, None));
        let new_chain = Arc::new(mk_chain_with_updates(12, 20, None));

        // Notification: chain reorged 12..20
        let notif = ExExNotification::ChainReorged { new: new_chain, old: old_chain };

        exex.handle_notification(notif, &sync_target).expect("handle chain re-orged");

        // State is set; sync loop will detect revert is beyond stored blocks
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(
            state,
            SyncTargetState::RevertThenSync { revert_to, sync_to: 20 }
            if revert_to.block.number == 12
        ));

        // Storage unchanged
        let latest = proofs.get_latest_block_number().expect("get latest block").expect("ok").0;
        assert_eq!(latest, 10);
    }

    #[tokio::test]
    async fn handle_notification_chain_reverted() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());
        store_blocks(1, 10, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let sync_target = SyncTarget::new();

        // Now the tip is 10, and we want to revert from block 9..10
        let old_chain = Arc::new(mk_chain_with_updates(9, 10, None));

        // Notification: chain reverted 9..10
        let notif = ExExNotification::ChainReverted { old: old_chain };

        exex.handle_notification(notif, &sync_target).expect("handle chain reverted");

        // Should have Revert state
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(
            state,
            SyncTargetState::Revert { revert_to }
            if revert_to.block.number == 9
        ));

        // Storage unchanged (sync loop handles the actual revert)
        let latest = proofs.get_latest_block_number().expect("get latest block").expect("ok").0;
        assert_eq!(latest, 10);
    }

    #[tokio::test]
    async fn handle_notification_chain_reverted_beyond_stored_blocks() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());
        store_blocks(1, 5, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        let sync_target = SyncTarget::new();

        // Now the tip is 5, and we want to revert from block 9..10
        let old_chain = Arc::new(mk_chain_with_updates(9, 10, None));

        // Notification: chain reverted 9..10
        let notif = ExExNotification::ChainReverted { old: old_chain };

        exex.handle_notification(notif, &sync_target).expect("handle chain reverted");

        // State is set; sync loop will detect revert is beyond stored blocks
        let state = sync_target.take_state().expect("should have pending state");
        assert!(matches!(
            state,
            SyncTargetState::Revert { revert_to }
            if revert_to.block.number == 9
        ));

        // Storage unchanged
        let latest = proofs.get_latest_block_number().expect("get latest block").expect("ok").0;
        assert_eq!(latest, 5);
    }

    #[tokio::test]
    async fn ensure_initialized_errors_on_storage_not_initialized() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());
        let _ = exex.ensure_initialized().expect_err("should return error");
    }

    #[tokio::test]
    async fn ensure_initialized_errors_when_prune_exceeds_threshold() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());

        for i in 1..1100 {
            proofs
                .store_trie_updates(
                    BlockWithParent::new(
                        hash_for_num(i - 1),
                        BlockNumHash::new(i, hash_for_num(i)),
                    ),
                    BlockStateDiff::default(),
                )
                .expect("store trie update");
        }

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());
        let _ = exex.ensure_initialized().expect_err("should return error");
    }

    #[tokio::test]
    async fn ensure_initialized_succeeds() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());
        exex.ensure_initialized().expect("should not return error");
    }

    #[tokio::test]
    async fn handle_notification_schedules_async_on_gap() {
        // MDBX proofs storage
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();

        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");

        let exex = build_test_exex(ctx, proofs.clone());

        // Notification: chain committed 5..10 (Blocks 1,2,3,4 are missing from storage)
        let new_chain = Arc::new(mk_chain_with_updates(5, 10, None));
        let notif = ExExNotification::ChainCommitted { new: new_chain };

        let sync_target = SyncTarget::new();

        // Process notification
        exex.handle_notification(notif, &sync_target)
            .expect("handle chain commit should return ok immediately");

        // Verify the sync target state was set
        let state = sync_target.take_state().expect("should have pending state");
        assert!(
            matches!(state, SyncTargetState::SyncUpTo { to: 10 }),
            "Should have scheduled sync to block 10"
        );

        // Verify Main Thread did NOT process it
        // Because we didn't spawn the actual worker thread in this test, storage should still be at
        // 0. This proves the 'handle_notification' returned instantly without doing the
        // heavy lifting.
        let latest = proofs.get_latest_block_number().expect("get").expect("ok");
        assert_eq!(latest.0, 0, "Main thread should not have processed the blocks synchronously");
    }

    /// Proves that cloning `LazyTrieData` (old behavior) keeps the `DeferredTrieData`
    /// handle alive — preventing the engine from freeing `ComputedTrieData` (including
    /// the cumulative `TrieInputSorted` overlay). Extracting just the sorted Arcs (new
    /// behavior) releases the handle immediately.
    #[test]
    fn lazy_trie_data_clone_retains_deferred_trie_data() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::{SortedTrieData, TrieInputSorted};

        let hashed_state = Arc::new(HashedPostStateSorted::default());
        let trie_updates = Arc::new(TrieUpdatesSorted::default());

        // Simulate the cumulative overlay that would be held in ComputedTrieData.
        let overlay = Arc::new(TrieInputSorted::default());

        let computed = reth_chain_state::ComputedTrieData::with_trie_input(
            Arc::clone(&hashed_state),
            Arc::clone(&trie_updates),
            B256::ZERO,
            Arc::clone(&overlay),
        );

        // Create a DeferredTrieData in Ready state (simulates post-execution).
        let deferred = DeferredTrieData::ready(computed);

        // Create a LazyTrieData::deferred closure capturing the handle —
        // this is exactly what blocks_to_chain() does.
        let deferred_clone = deferred.clone();
        let lazy = LazyTrieData::deferred(move || {
            let data = deferred_clone.wait_cloned();
            SortedTrieData::new(data.hashed_state, data.trie_updates)
        });

        // Clone the LazyTrieData (old behavior: what handle_chain_committed used to do).
        let lazy_clone = lazy.clone();

        // Drop the original LazyTrieData and the original DeferredTrieData.
        // This simulates the engine dropping the block from CanonicalInMemoryState.
        drop(lazy);
        drop(deferred);

        // The overlay Arc should still be alive because lazy_clone holds:
        //   lazy_clone.compute -> Arc<dyn Fn> -> DeferredTrieData -> Arc<Mutex<Ready(ComputedTrieData)>> -> overlay
        assert_eq!(
            Arc::strong_count(&overlay), 2,
            "Old behavior: LazyTrieData clone keeps the overlay alive (refcount should be 2: \
             one in ComputedTrieData via DeferredTrieData, one our local)"
        );

        // Now simulate the new behavior: call .get() to extract sorted data,
        // then keep only the Arcs.
        let sorted = lazy_clone.get();
        let kept_hashed = Arc::clone(&sorted.hashed_state);
        let kept_updates = Arc::clone(&sorted.trie_updates);

        // Drop the LazyTrieData clone entirely.
        drop(lazy_clone);

        // The overlay should now be freed — only our local Arc remains.
        assert_eq!(
            Arc::strong_count(&overlay), 1,
            "New behavior: after dropping LazyTrieData, the overlay is freed (refcount should be 1: \
             only our local reference)"
        );

        // The sorted data we actually need is still alive.
        assert!(Arc::strong_count(&kept_hashed) >= 1);
        assert!(Arc::strong_count(&kept_updates) >= 1);
    }

    /// Proves that a chain of `DeferredTrieData` ancestors is kept alive when
    /// any descendant's `LazyTrieData` clone exists. This simulates the
    /// engine's in-memory block chain where each block's `DeferredTrieData`
    /// holds references to all ancestor blocks' deferred handles.
    #[test]
    fn ancestor_chain_retained_by_lazy_clone() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::{HashedPostState, SortedTrieData, TrieInputSorted, updates::TrieUpdates};

        let ancestor_overlay = Arc::new(TrieInputSorted::default());

        // Block A: root of the chain, with a cumulative overlay.
        let block_a = DeferredTrieData::ready(
            reth_chain_state::ComputedTrieData::with_trie_input(
                Arc::new(HashedPostStateSorted::default()),
                Arc::new(TrieUpdatesSorted::default()),
                B256::ZERO,
                Arc::clone(&ancestor_overlay),
            ),
        );

        // Block B: pending, with block_a as ancestor.
        let block_b = DeferredTrieData::pending(
            Arc::new(HashedPostState::default()),
            Arc::new(TrieUpdates::default()),
            B256::ZERO,
            vec![block_a.clone()],
        );

        // Simulate blocks_to_chain: create LazyTrieData for block B.
        let block_b_clone = block_b.clone();
        let lazy_b = LazyTrieData::deferred(move || {
            let data = block_b_clone.wait_cloned();
            SortedTrieData::new(data.hashed_state, data.trie_updates)
        });

        // Clone it (old behavior).
        let lazy_b_clone = lazy_b.clone();

        // Engine drops everything: original lazy, block_a, block_b.
        drop(lazy_b);
        drop(block_a);
        drop(block_b);

        // Block A's overlay is still alive via:
        //   lazy_b_clone -> closure -> DeferredTrieData(block_b) -> Pending.ancestors -> DeferredTrieData(block_a) -> Ready -> overlay
        assert_eq!(
            Arc::strong_count(&ancestor_overlay), 2,
            "Ancestor overlay kept alive through pending descendant's LazyTrieData clone"
        );

        // Call .get() to trigger computation, then drop the LazyTrieData.
        let sorted = lazy_b_clone.get();
        let _kept = Arc::clone(&sorted.hashed_state);
        drop(lazy_b_clone);

        // After wait_cloned(), Pending transitions to Ready which drops ancestors.
        // Then dropping LazyTrieData drops the closure + DeferredTrieData.
        assert_eq!(
            Arc::strong_count(&ancestor_overlay), 1,
            "After extracting sorted data and dropping LazyTrieData, ancestor overlay is freed"
        );
    }

    /// Simulates the SyncTarget holding multiple entries (old behavior) and
    /// proves each entry independently retains its overlay. Dropping entries
    /// one-by-one frees overlays incrementally.
    #[test]
    fn sync_target_entries_independently_retain_overlays() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::{SortedTrieData, TrieInputSorted};

        let num_blocks = 10;
        let mut overlays = Vec::new();
        let mut lazy_clones = Vec::new();

        for _ in 0..num_blocks {
            let overlay = Arc::new(TrieInputSorted::default());
            overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::ZERO,
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });
            lazy_clones.push(lazy.clone());
            // Drop original lazy + deferred (engine side).
            drop(lazy);
            drop(deferred);
        }

        // All overlays held alive by lazy_clones.
        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 2,
                "Overlay {i} should be retained by its LazyTrieData clone"
            );
        }

        // Drop entries one-by-one (simulates SyncTarget eviction via pop_first).
        for i in 0..num_blocks {
            drop(lazy_clones.remove(0));
            assert_eq!(
                Arc::strong_count(&overlays[i]), 1,
                "Overlay {i} should be freed after its LazyTrieData clone is dropped"
            );
            // Remaining overlays still held.
            for (j, overlay) in overlays.iter().enumerate().skip(i + 1) {
                assert_eq!(
                    Arc::strong_count(overlay), 2,
                    "Overlay {j} should still be retained"
                );
            }
        }
    }

    /// Proves that the new behavior (eagerly extracting sorted data into
    /// `CachedBlockTrieData`) releases overlays immediately, even when
    /// the cached data itself is kept alive.
    #[test]
    fn cached_block_trie_data_does_not_retain_overlay() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::{SortedTrieData, TrieInputSorted};

        let overlay = Arc::new(TrieInputSorted::default());

        let deferred = DeferredTrieData::ready(
            reth_chain_state::ComputedTrieData::with_trie_input(
                Arc::new(HashedPostStateSorted::default()),
                Arc::new(TrieUpdatesSorted::default()),
                B256::ZERO,
                Arc::clone(&overlay),
            ),
        );

        // Simulate blocks_to_chain creating LazyTrieData.
        let deferred_clone = deferred.clone();
        let lazy = LazyTrieData::deferred(move || {
            let data = deferred_clone.wait_cloned();
            SortedTrieData::new(data.hashed_state, data.trie_updates)
        });

        // New behavior: eagerly extract sorted data into CachedBlockTrieData.
        let sorted = lazy.get();
        let cached = CachedBlockTrieData {
            block_with_parent: BlockWithParent::new(
                B256::ZERO,
                NumHash::new(1, B256::ZERO),
            ),
            hashed_state: Arc::clone(&sorted.hashed_state),
            trie_updates: Arc::clone(&sorted.trie_updates),
        };

        // Drop everything except the cached data.
        drop(lazy);
        drop(deferred);

        // Overlay is freed — CachedBlockTrieData doesn't hold it.
        assert_eq!(
            Arc::strong_count(&overlay), 1,
            "CachedBlockTrieData does not retain the overlay"
        );

        // The cached data is still usable.
        assert!(Arc::strong_count(&cached.hashed_state) >= 1);
        assert!(Arc::strong_count(&cached.trie_updates) >= 1);
    }

    /// Builds a large `TrieInputSorted` overlay to simulate real-world cumulative
    /// state. Each entry is a (B256, Account) pair — 32 bytes key + ~80 bytes value.
    fn build_large_overlay(num_accounts: usize) -> reth_trie::TrieInputSorted {
        use reth_primitives_traits::Account;
        use reth_trie::TrieInputSorted;

        let accounts: Vec<(B256, Option<Account>)> = (0..num_accounts)
            .map(|i| {
                let mut key = [0u8; 32];
                key[0..8].copy_from_slice(&(i as u64).to_be_bytes());
                (B256::new(key), Some(Account { nonce: i as u64, ..Default::default() }))
            })
            .collect();
        let state = Arc::new(HashedPostStateSorted::new(accounts, Default::default()));
        let nodes = Arc::new(TrieUpdatesSorted::default());
        TrieInputSorted::new(nodes, state, Default::default())
    }

    /// Measures the memory impact of old vs new behavior with realistic overlay sizes.
    ///
    /// Old behavior: 100 `LazyTrieData` clones retain 100 independent cumulative overlays.
    /// New behavior: 100 `CachedBlockTrieData` entries hold only sorted per-block data,
    /// releasing the overlays immediately.
    ///
    /// Each overlay contains 10,000 account entries (~1.1 MB each). With 100 entries:
    /// - Old behavior retains ~110 MB of overlays
    /// - New behavior retains ~0 MB of overlays
    #[test]
    fn memory_impact_old_vs_new_behavior() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let accounts_per_overlay = 10_000;
        let num_blocks = 100;

        // --- Old behavior: LazyTrieData clones ---
        let mut old_overlays = Vec::new();
        let mut old_lazy_clones: Vec<LazyTrieData> = Vec::new();

        for _ in 0..num_blocks {
            let overlay = Arc::new(build_large_overlay(accounts_per_overlay));
            old_overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::ZERO,
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });
            old_lazy_clones.push(lazy.clone());
            drop(lazy);
            drop(deferred);
        }

        // Verify all overlays are retained.
        let old_retained = old_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) > 1)
            .count();
        assert_eq!(old_retained, num_blocks, "Old behavior: all {num_blocks} overlays retained");

        // Drop all LazyTrieData clones (simulates switching to new behavior).
        drop(old_lazy_clones);

        // All overlays freed.
        let old_freed = old_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) == 1)
            .count();
        assert_eq!(old_freed, num_blocks, "After dropping old clones: all overlays freed");

        drop(old_overlays);

        // --- New behavior: eagerly extracted CachedBlockTrieData ---
        let mut new_overlays = Vec::new();
        let mut new_cached_entries: Vec<CachedBlockTrieData> = Vec::new();

        for i in 0..num_blocks {
            let overlay = Arc::new(build_large_overlay(accounts_per_overlay));
            new_overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::ZERO,
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });

            // New behavior: eagerly extract, then drop LazyTrieData.
            let sorted = lazy.get();
            new_cached_entries.push(CachedBlockTrieData {
                block_with_parent: BlockWithParent::new(
                    B256::ZERO,
                    NumHash::new(i as u64, B256::ZERO),
                ),
                hashed_state: Arc::clone(&sorted.hashed_state),
                trie_updates: Arc::clone(&sorted.trie_updates),
            });
            drop(lazy);
            drop(deferred);
        }

        // Verify NO overlays are retained.
        let new_retained = new_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) > 1)
            .count();
        assert_eq!(new_retained, 0, "New behavior: zero overlays retained");

        // Cached entries are still usable.
        for entry in &new_cached_entries {
            assert!(Arc::strong_count(&entry.hashed_state) >= 1);
            assert!(Arc::strong_count(&entry.trie_updates) >= 1);
        }
    }

    /// Simulates growing cumulative overlays (each block extends the previous)
    /// and proves the old behavior retains ALL cumulative data while new behavior
    /// only keeps per-block sorted data.
    #[test]
    fn growing_cumulative_overlays_freed_by_new_behavior() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let num_blocks = 20;
        let accounts_per_block = 1_000;

        // Simulate growing cumulative overlays: block N's overlay contains
        // state changes from ALL blocks [0..N].
        let mut overlays = Vec::new();
        let mut lazy_clones: Vec<LazyTrieData> = Vec::new();

        for block_idx in 0..num_blocks {
            // Each overlay grows: block 0 has 1000, block 1 has 2000, etc.
            let cumulative_accounts = (block_idx + 1) * accounts_per_block;
            let overlay = Arc::new(build_large_overlay(cumulative_accounts));
            overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::ZERO,
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });
            lazy_clones.push(lazy.clone());
            drop(lazy);
            drop(deferred);
        }

        // All 20 growing overlays retained — total is O(N^2) memory:
        // overlay[0] = 1000 accounts, overlay[1] = 2000, ..., overlay[19] = 20000
        // Sum = 1000 * (1+2+...+20) = 210,000 account entries held
        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 2,
                "Growing overlay {i} retained by old behavior"
            );
        }

        // New behavior: extract and drop.
        let mut cached_entries: Vec<CachedBlockTrieData> = Vec::new();
        for (i, lazy) in lazy_clones.into_iter().enumerate() {
            let sorted = lazy.get();
            cached_entries.push(CachedBlockTrieData {
                block_with_parent: BlockWithParent::new(
                    B256::ZERO,
                    NumHash::new(i as u64, B256::ZERO),
                ),
                hashed_state: Arc::clone(&sorted.hashed_state),
                trie_updates: Arc::clone(&sorted.trie_updates),
            });
            // lazy dropped here at end of iteration
        }

        // All overlays freed.
        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 1,
                "Growing overlay {i} freed after extracting sorted data"
            );
        }

        // Cached entries still usable.
        assert_eq!(cached_entries.len(), num_blocks);
    }

    /// Creates a chain where trie data uses `DeferredTrieData` closures (like the
    /// real engine's `blocks_to_chain`), each backed by a large cumulative overlay.
    /// Returns the chain and the overlay Arcs so callers can verify refcounts.
    fn mk_chain_with_deferred_overlays(
        from: u64,
        to: u64,
        accounts_per_overlay: usize,
    ) -> (
        Chain<reth_ethereum_primitives::EthPrimitives>,
        Vec<Arc<reth_trie::TrieInputSorted>>,
    ) {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let mut blocks: Vec<RecoveredBlock<Block>> = Vec::new();
        let mut trie_data = BTreeMap::new();
        let mut overlays = Vec::new();

        for n in from..=to {
            blocks.push(mk_block(n));

            let overlay = Arc::new(build_large_overlay(accounts_per_overlay));
            overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::ZERO,
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });
            trie_data.insert(n, lazy);
        }

        let execution_outcome: ExecutionOutcome<Receipt> = ExecutionOutcome {
            bundle: Default::default(),
            receipts: Vec::new(),
            requests: Vec::new(),
            first_block: from,
        };

        (Chain::new(blocks, execution_outcome, trie_data), overlays)
    }

    /// Integration test: notifications with deferred trie data are handled
    /// correctly and overlays are freed after caching into SyncTarget.
    #[tokio::test]
    async fn handle_notification_frees_deferred_overlays() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());
        store_blocks(1, 5, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs);
        let sync_target = SyncTarget::new();

        let (chain, overlays) = mk_chain_with_deferred_overlays(6, 10, 5_000);

        // Overlays have refcount 2: our local + DeferredTrieData inside LazyTrieData.
        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 2,
                "Overlay {i} starts with refcount 2"
            );
        }

        let notification = ExExNotification::ChainCommitted { new: Arc::new(chain) };
        exex.handle_notification(notification, &sync_target)
            .expect("handle_notification");

        // After handle_notification (new behavior): the LazyTrieData was consumed
        // via .get() and only sorted Arcs are kept in SyncTarget. The DeferredTrieData
        // closure is dropped, releasing the overlays.
        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 1,
                "Overlay {i} freed after handle_notification (only local ref remains)"
            );
        }

        assert_eq!(sync_target.cache_len(), 5);
    }

    /// Integration test: consuming SyncTarget entries (simulating sync_forward)
    /// doesn't resurrect overlay references.
    #[tokio::test]
    async fn sync_target_take_does_not_retain_overlays() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());
        store_blocks(1, 5, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs);
        let sync_target = SyncTarget::new();

        let (chain, overlays) = mk_chain_with_deferred_overlays(6, 10, 5_000);

        let notification = ExExNotification::ChainCommitted { new: Arc::new(chain) };
        exex.handle_notification(notification, &sync_target)
            .expect("handle_notification");

        for overlay in &overlays {
            assert_eq!(Arc::strong_count(overlay), 1);
        }

        // Consume entries from SyncTarget (simulates sync_forward's take() calls).
        for n in 6..=10 {
            let entry = sync_target.take(n).expect("block should be in cache");
            let _hashed = (*entry.hashed_state).clone();
            let _updates = (*entry.trie_updates).clone();
        }

        assert_eq!(sync_target.cache_len(), 0);

        for (i, overlay) in overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 1,
                "Overlay {i} still freed after consuming from SyncTarget"
            );
        }
    }

    /// Integration test: SyncTarget eviction via capacity overflow frees overlays.
    #[tokio::test]
    async fn sync_target_eviction_frees_overlays() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs);
        let sync_target = SyncTarget::new();

        let mut all_overlays = Vec::new();
        let batch_size = 50u64;
        let total_blocks = 1030u64;
        let mut block_start = 1u64;

        while block_start <= total_blocks {
            let batch_end = (block_start + batch_size - 1).min(total_blocks);
            let (chain, overlays) =
                mk_chain_with_deferred_overlays(block_start, batch_end, 100);
            all_overlays.extend(overlays);

            let notification =
                ExExNotification::ChainCommitted { new: Arc::new(chain) };
            exex.handle_notification(notification, &sync_target)
                .expect("handle_notification");

            block_start = batch_end + 1;
        }

        for (i, overlay) in all_overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 1,
                "Overlay {i} freed immediately after handle_notification"
            );
        }

        assert!(sync_target.cache_len() <= 1024);
    }

    /// Proves that `LazyTrieData.get()` does NOT drop the compute closure.
    /// Even after the `OnceLock` is populated, the closure (and its captured
    /// `DeferredTrieData`) remains alive. This is the core reth behavior
    /// that causes memory retention with the old approach.
    #[test]
    fn lazy_trie_data_get_does_not_drop_closure() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let overlay = Arc::new(reth_trie::TrieInputSorted::default());

        let deferred = DeferredTrieData::ready(
            reth_chain_state::ComputedTrieData::with_trie_input(
                Arc::new(HashedPostStateSorted::default()),
                Arc::new(TrieUpdatesSorted::default()),
                B256::ZERO,
                Arc::clone(&overlay),
            ),
        );

        let deferred_clone = deferred.clone();
        let lazy = LazyTrieData::deferred(move || {
            let data = deferred_clone.wait_cloned();
            SortedTrieData::new(data.hashed_state, data.trie_updates)
        });
        drop(deferred);

        // Before get(): overlay alive via closure.
        assert_eq!(Arc::strong_count(&overlay), 2);

        // Call get() — populates the OnceLock.
        let _sorted = lazy.get();

        // After get(): overlay STILL alive — closure is NOT dropped.
        assert_eq!(
            Arc::strong_count(&overlay), 2,
            "get() does not drop the compute closure; overlay still retained"
        );

        drop(lazy);
        assert_eq!(Arc::strong_count(&overlay), 1, "Only dropping LazyTrieData frees it");
    }

    /// Integration test: reorg path (`ChainReorged`) also frees overlays for
    /// the new chain's trie data.
    #[tokio::test]
    async fn handle_reorg_frees_deferred_overlays() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());
        store_blocks(1, 5, &proofs);

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs);
        let sync_target = SyncTarget::new();

        // First, commit blocks 6-10.
        let (old_chain, _old_overlays) = mk_chain_with_deferred_overlays(6, 10, 1_000);
        let notification = ExExNotification::ChainCommitted { new: Arc::new(old_chain) };
        exex.handle_notification(notification, &sync_target).expect("commit");

        // Now reorg: old chain is 6-10, new chain is 6-8 (shorter fork).
        let (old_chain_for_reorg, _) = mk_chain_with_deferred_overlays(6, 10, 1_000);
        let (new_chain, new_overlays) = mk_chain_with_deferred_overlays(6, 8, 2_000);

        for overlay in &new_overlays {
            assert_eq!(Arc::strong_count(overlay), 2);
        }

        let notification = ExExNotification::ChainReorged {
            old: Arc::new(old_chain_for_reorg),
            new: Arc::new(new_chain),
        };
        exex.handle_notification(notification, &sync_target).expect("reorg");

        // New chain overlays freed immediately.
        for (i, overlay) in new_overlays.iter().enumerate() {
            assert_eq!(
                Arc::strong_count(overlay), 1,
                "Reorg new chain overlay {i} freed after handle_notification"
            );
        }
    }

    /// Proves that when the engine and exex share a `DeferredTrieData` handle,
    /// the engine dropping its reference doesn't free the overlay as long as
    /// the exex's `LazyTrieData` clone exists (old behavior). With the new
    /// behavior, the overlay is freed as soon as the sorted data is extracted.
    #[test]
    fn shared_deferred_handle_engine_drops_first() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let overlay = Arc::new(reth_trie::TrieInputSorted::default());

        // Engine creates the DeferredTrieData (simulates ExecutedBlock).
        let engine_handle = DeferredTrieData::ready(
            reth_chain_state::ComputedTrieData::with_trie_input(
                Arc::new(HashedPostStateSorted::default()),
                Arc::new(TrieUpdatesSorted::default()),
                B256::ZERO,
                Arc::clone(&overlay),
            ),
        );

        // ExEx receives a clone via blocks_to_chain's LazyTrieData closure.
        let exex_handle = engine_handle.clone();
        let lazy = LazyTrieData::deferred(move || {
            let data = exex_handle.wait_cloned();
            SortedTrieData::new(data.hashed_state, data.trie_updates)
        });
        let lazy_clone = lazy.clone();
        drop(lazy);

        // Engine persists the block and drops its handle.
        drop(engine_handle);

        // Old behavior: overlay still alive because exex's LazyTrieData clone holds it.
        assert_eq!(
            Arc::strong_count(&overlay), 2,
            "Engine dropped handle, but exex's LazyTrieData clone keeps overlay alive"
        );

        // New behavior: extract sorted data, drop LazyTrieData.
        let sorted = lazy_clone.get();
        let _kept_hashed = Arc::clone(&sorted.hashed_state);
        let _kept_updates = Arc::clone(&sorted.trie_updates);
        drop(lazy_clone);

        assert_eq!(
            Arc::strong_count(&overlay), 1,
            "After extracting and dropping, overlay is freed"
        );
    }

    /// End-to-end simulation of sustained catch-up: the exex receives many
    /// batches of notifications (like during chain sync), processes them into
    /// SyncTarget, and entries are consumed by the sync loop. Verifies that
    /// overlays from ALL batches are freed — no cumulative memory growth.
    #[tokio::test]
    async fn e2e_sustained_catchup_no_memory_growth() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs.clone());
        let sync_target = SyncTarget::new();

        let batches = 20;
        let blocks_per_batch = 50u64;
        let accounts_per_overlay = 5_000;
        let mut all_overlays: Vec<Arc<reth_trie::TrieInputSorted>> = Vec::new();

        for batch in 0..batches {
            let start = batch * blocks_per_batch + 1;
            let end = start + blocks_per_batch - 1;

            let (chain, overlays) =
                mk_chain_with_deferred_overlays(start, end, accounts_per_overlay);
            all_overlays.extend(overlays);

            let notification =
                ExExNotification::ChainCommitted { new: Arc::new(chain) };
            exex.handle_notification(notification, &sync_target)
                .expect("handle_notification");

            // Simulate sync loop consuming the batch.
            for n in start..=end {
                if let Some(entry) = sync_target.take(n) {
                    let _h = (*entry.hashed_state).clone();
                    let _u = (*entry.trie_updates).clone();
                }
            }
        }

        // Total: 1000 blocks processed in 20 batches.
        // ALL overlays should be freed — no cumulative memory growth.
        let retained = all_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) > 1)
            .count();
        assert_eq!(
            retained, 0,
            "After processing {batches} batches ({} blocks), zero overlays retained",
            batches * blocks_per_batch
        );

        assert_eq!(sync_target.cache_len(), 0);
    }

    /// End-to-end test: notifications arrive faster than the sync loop consumes
    /// them (simulates the slow-write scenario). SyncTarget fills up, older
    /// entries are evicted, but ALL overlays are still freed.
    #[tokio::test]
    async fn e2e_notifications_faster_than_sync() {
        let dir = tempdir_path();
        let store = Arc::new(MdbxProofsStorage::new(dir.as_path()).expect("env"));
        let proofs: OpProofsStorage<Arc<MdbxProofsStorage>> = Arc::clone(&store).into();
        init_storage(proofs.clone());

        let (ctx, _handle) =
            reth_exex_test_utils::test_exex_context().await.expect("exex test context");
        let exex = build_test_exex(ctx, proofs.clone());
        let sync_target = SyncTarget::new();

        let mut all_overlays = Vec::new();
        let total_notifications = 30;
        let blocks_per_notification = 10u64;

        // Send 30 notifications (300 blocks total) without consuming any.
        for batch in 0..total_notifications {
            let start = batch * blocks_per_notification + 1;
            let end = start + blocks_per_notification - 1;

            let (chain, overlays) =
                mk_chain_with_deferred_overlays(start, end, 1_000);
            all_overlays.extend(overlays);

            let notification =
                ExExNotification::ChainCommitted { new: Arc::new(chain) };
            exex.handle_notification(notification, &sync_target)
                .expect("handle_notification");
        }

        // All 300 overlays freed immediately (new behavior), even though
        // SyncTarget still holds cached entries.
        let retained = all_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) > 1)
            .count();
        assert_eq!(retained, 0, "All overlays freed despite no sync consumption");

        // SyncTarget has entries (up to capacity).
        assert!(sync_target.cache_len() > 0);

        // Now consume everything.
        for n in 1..=(total_notifications * blocks_per_notification) {
            sync_target.take(n);
        }

        // Still zero retained.
        let retained_after = all_overlays
            .iter()
            .filter(|o| Arc::strong_count(o) > 1)
            .count();
        assert_eq!(retained_after, 0, "Still zero after full consumption");
    }

    /// Contrasts old vs new behavior in an e2e-style scenario: sends many
    /// notifications and caches them using both approaches, proving that
    /// the old `LazyTrieData::clone()` approach retains overlays while the
    /// new eager-extraction approach does not.
    #[test]
    fn e2e_old_vs_new_overlay_retention() {
        use reth_chain_state::DeferredTrieData;
        use reth_trie::SortedTrieData;

        let total_blocks = 200;
        let accounts_per_overlay = 2_000;

        // --- Old behavior: clone LazyTrieData into a Vec (simulating SyncTarget) ---
        let mut old_cache: Vec<LazyTrieData> = Vec::new();
        let mut old_overlays = Vec::new();

        for i in 0..total_blocks {
            let overlay = Arc::new(build_large_overlay(accounts_per_overlay));
            old_overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::new([i as u8; 32]),
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });

            // Old behavior: cache the LazyTrieData clone.
            old_cache.push(lazy.clone());

            // Engine drops its side.
            drop(lazy);
            drop(deferred);
        }

        let old_retained = old_overlays.iter().filter(|o| Arc::strong_count(o) > 1).count();
        assert_eq!(
            old_retained, total_blocks,
            "Old behavior: ALL {total_blocks} overlays retained while cache is alive"
        );

        // Simulate sync loop consuming entries (like process_block).
        for lazy in &old_cache {
            let _sorted = lazy.get();
        }

        // Even after get(), overlays are STILL retained.
        let old_retained_after_get =
            old_overlays.iter().filter(|o| Arc::strong_count(o) > 1).count();
        assert_eq!(
            old_retained_after_get, total_blocks,
            "Old behavior: overlays STILL retained even after get() — closure not dropped"
        );

        // Only dropping the cache frees them.
        drop(old_cache);
        let old_freed = old_overlays.iter().filter(|o| Arc::strong_count(o) == 1).count();
        assert_eq!(old_freed, total_blocks, "Old behavior: overlays freed after cache dropped");

        // --- New behavior: eagerly extract sorted data (like CachedBlockTrieData) ---
        let mut new_cache: Vec<(Arc<HashedPostStateSorted>, Arc<TrieUpdatesSorted>)> = Vec::new();
        let mut new_overlays = Vec::new();

        for i in 0..total_blocks {
            let overlay = Arc::new(build_large_overlay(accounts_per_overlay));
            new_overlays.push(Arc::clone(&overlay));

            let deferred = DeferredTrieData::ready(
                reth_chain_state::ComputedTrieData::with_trie_input(
                    Arc::new(HashedPostStateSorted::default()),
                    Arc::new(TrieUpdatesSorted::default()),
                    B256::new([i as u8; 32]),
                    overlay,
                ),
            );

            let deferred_clone = deferred.clone();
            let lazy = LazyTrieData::deferred(move || {
                let data = deferred_clone.wait_cloned();
                SortedTrieData::new(data.hashed_state, data.trie_updates)
            });

            // New behavior: extract immediately, drop LazyTrieData.
            let sorted = lazy.get();
            new_cache.push((Arc::clone(&sorted.hashed_state), Arc::clone(&sorted.trie_updates)));
            drop(lazy);
            drop(deferred);
        }

        // Overlays freed IMMEDIATELY — even while cache is alive.
        let new_retained = new_overlays.iter().filter(|o| Arc::strong_count(o) > 1).count();
        assert_eq!(
            new_retained, 0,
            "New behavior: ALL overlays freed immediately, cache holds only sorted data"
        );

        // Cache is still usable.
        assert_eq!(new_cache.len(), total_blocks);
    }
}
