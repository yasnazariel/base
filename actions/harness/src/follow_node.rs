use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use base_common_consensus::BaseBlock;
use base_common_provider::BaseEngineApi;
use base_consensus_engine::EngineForkchoiceVersion;
use base_consensus_genesis::RollupConfig;
use base_consensus_safedb::{
    SafeDB, SafeDBError, SafeDBReader, SafeHeadListener, SafeHeadResponse,
};
use base_protocol::{BlockInfo, L2BlockInfo};

use crate::ActionEngineClient;

/// Shared in-memory L2 block store used as the source node for a [`TestFollowNode`].
///
/// Stores fully-formed [`BaseBlock`]s built by a [`crate::L2Sequencer`] and exposes
/// them by block number.  The safe and finalized head numbers are updated by the
/// test directly via [`set_safe_number`] and [`set_finalized_number`], defaulting
/// to `0`.
///
/// [`set_safe_number`]: ActionL2SourceBridge::set_safe_number
/// [`set_finalized_number`]: ActionL2SourceBridge::set_finalized_number
#[derive(Debug, Clone, Default)]
pub struct ActionL2SourceBridge {
    inner: Arc<Mutex<SourceBridgeInner>>,
}

#[derive(Debug, Default)]
struct SourceBridgeInner {
    blocks: HashMap<u64, BaseBlock>,
    safe_number: u64,
    finalized_number: u64,
}

impl ActionL2SourceBridge {
    /// Create an empty source bridge.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a block into the bridge, making it available to the follow node.
    ///
    /// The block's number is used as the map key. Subsequent calls with the same
    /// block number overwrite the previous entry.
    pub fn push(&self, block: BaseBlock) {
        let number = block.header.number;
        self.inner.lock().expect("source bridge lock poisoned").blocks.insert(number, block);
    }

    /// Return the highest block number currently in the bridge, or `0` when empty.
    pub fn latest_number(&self) -> u64 {
        self.inner
            .lock()
            .expect("source bridge lock poisoned")
            .blocks
            .keys()
            .max()
            .copied()
            .unwrap_or(0)
    }

    /// Return the current safe head number.
    pub fn safe_number(&self) -> u64 {
        self.inner.lock().expect("source bridge lock poisoned").safe_number
    }

    /// Return the current finalized head number.
    pub fn finalized_number(&self) -> u64 {
        self.inner.lock().expect("source bridge lock poisoned").finalized_number
    }

    /// Advance the source's safe head to `n`.
    pub fn set_safe_number(&self, n: u64) {
        self.inner.lock().expect("source bridge lock poisoned").safe_number = n;
    }

    /// Advance the source's finalized head to `n`.
    pub fn set_finalized_number(&self, n: u64) {
        self.inner.lock().expect("source bridge lock poisoned").finalized_number = n;
    }

    /// Return the block at `number`, if present.
    pub fn get_block(&self, number: u64) -> Option<BaseBlock> {
        self.inner.lock().expect("source bridge lock poisoned").blocks.get(&number).cloned()
    }
}

/// A deterministic in-process follow node for action tests.
///
/// `TestFollowNode` replicates the behavior of the production [`FollowNode`]
/// without the actor message-passing infrastructure.  It drives block execution
/// directly against an [`ActionEngineClient`] and records safe-head mappings in
/// a real [`SafeDB`].
///
/// # Usage
///
/// 1. Build L2 blocks via [`crate::L2Sequencer`] and push each into the
///    [`ActionL2SourceBridge`] held by this node:
///    ```rust,ignore
///    let block = sequencer.build_next_block_with_single_transaction().await;
///    follow_node.source().push(block);
///    ```
/// 2. Sync the follow node up to the desired block:
///    ```rust,ignore
///    follow_node.sync_up_to(3).await;
///    ```
/// 3. Assert head advancement and safe-DB entries:
///    ```rust,ignore
///    assert_eq!(follow_node.l2_unsafe_number(), 3);
///    follow_node.safe_head_at_l1(l1_origin_number).await.unwrap();
///    ```
///
/// [`FollowNode`]: base_consensus_service::FollowNode
#[derive(Debug)]
pub struct TestFollowNode {
    engine: ActionEngineClient,
    safe_db: Arc<SafeDB>,
    _safedb_dir: tempfile::TempDir,
    rollup_config: Arc<RollupConfig>,
    source: ActionL2SourceBridge,
    unsafe_head: L2BlockInfo,
    safe_head: L2BlockInfo,
    finalized_head: L2BlockInfo,
}

impl TestFollowNode {
    /// Construct a new `TestFollowNode` from pre-built components.
    pub fn new(
        engine: ActionEngineClient,
        source: ActionL2SourceBridge,
        genesis_head: L2BlockInfo,
        rollup_config: Arc<RollupConfig>,
    ) -> Self {
        let safedb_dir =
            tempfile::TempDir::new().expect("TestFollowNode: failed to create safedb temp dir");
        let safe_db = Arc::new(
            SafeDB::open(safedb_dir.path().join("safedb"))
                .expect("TestFollowNode: failed to open safedb"),
        );
        Self {
            engine,
            safe_db,
            _safedb_dir: safedb_dir,
            rollup_config,
            source,
            unsafe_head: genesis_head,
            safe_head: genesis_head,
            finalized_head: genesis_head,
        }
    }

    /// Return a shared reference to the source bridge.
    pub fn source(&self) -> &ActionL2SourceBridge {
        &self.source
    }

    /// Return the current L2 unsafe head.
    pub const fn l2_unsafe(&self) -> L2BlockInfo {
        self.unsafe_head
    }

    /// Return the current L2 unsafe head block number.
    pub const fn l2_unsafe_number(&self) -> u64 {
        self.unsafe_head.block_info.number
    }

    /// Return the current L2 safe head.
    pub const fn l2_safe(&self) -> L2BlockInfo {
        self.safe_head
    }

    /// Return the current L2 safe head block number.
    pub const fn l2_safe_number(&self) -> u64 {
        self.safe_head.block_info.number
    }

    /// Return the current L2 finalized head.
    pub const fn l2_finalized(&self) -> L2BlockInfo {
        self.finalized_head
    }

    /// Return the current L2 finalized head block number.
    pub const fn l2_finalized_number(&self) -> u64 {
        self.finalized_head.block_info.number
    }

    /// Query the safe head recorded for a given L1 block number from the persistent [`SafeDB`].
    pub async fn safe_head_at_l1(
        &self,
        l1_block_num: u64,
    ) -> Result<SafeHeadResponse, SafeDBError> {
        self.safe_db.safe_head_at_l1(l1_block_num).await
    }

    /// Process the next sequential unsafe block from the source.
    ///
    /// Fetches block `unsafe_head + 1` from the source bridge, inserts it into
    /// the engine via `new_payload`, issues a `fork_choice_updated` with the new
    /// unsafe head, and advances `unsafe_head`.
    ///
    /// Returns `true` when a block was processed; `false` when no block is
    /// available at the next number.
    pub async fn sync_next_block(&mut self) -> bool {
        let next = self.unsafe_head.block_info.number + 1;
        let Some(block) = self.source.get_block(next) else {
            return false;
        };

        // Execute the block through the engine. execute_block preserves parent_beacon_block_root
        // (required for Ecotone+) and re-executes from the block's own fields.
        let computed_hash =
            self.engine.execute_block(&block).expect("TestFollowNode: execute_block failed");

        // Decode L2BlockInfo to track l1_origin and seq_num.
        let l2_info = L2BlockInfo::from_block_and_genesis(&block, &self.rollup_config.genesis)
            .expect("TestFollowNode: failed to decode L2BlockInfo from block");

        self.unsafe_head = L2BlockInfo {
            block_info: BlockInfo {
                hash: computed_hash,
                number: block.header.number,
                parent_hash: block.header.parent_hash,
                timestamp: block.header.timestamp,
            },
            l1_origin: l2_info.l1_origin,
            seq_num: l2_info.seq_num,
        };

        // Issue FCU to advance the canonical unsafe head without changing safe/finalized.
        let timestamp = block.header.timestamp;
        let fcu = ForkchoiceState {
            head_block_hash: computed_hash,
            safe_block_hash: self.safe_head.block_info.hash,
            finalized_block_hash: self.finalized_head.block_info.hash,
        };
        match EngineForkchoiceVersion::from_cfg(&self.rollup_config, timestamp) {
            EngineForkchoiceVersion::V2 => {
                self.engine
                    .fork_choice_updated_v2(fcu, None)
                    .await
                    .expect("TestFollowNode: fcu_v2 failed");
            }
            EngineForkchoiceVersion::V3 => {
                self.engine
                    .fork_choice_updated_v3(fcu, None)
                    .await
                    .expect("TestFollowNode: fcu_v3 failed");
            }
        }

        true
    }

    /// Sync all blocks up to `target` from the source, then update safe and
    /// finalized heads.
    ///
    /// Processes each block sequentially.  After reaching `target`,
    /// [`update_safe_and_finalized`] is called once to write the safe-head
    /// entry to [`SafeDB`] and issue the consolidated FCU.
    ///
    /// [`update_safe_and_finalized`]: TestFollowNode::update_safe_and_finalized
    pub async fn sync_up_to(&mut self, target: u64) {
        while self.unsafe_head.block_info.number < target {
            if !self.sync_next_block().await {
                break;
            }
        }
        self.update_safe_and_finalized().await;
    }

    /// Sync all available blocks from the source and update safe/finalized heads.
    pub async fn sync_to_latest(&mut self) {
        let target = self.source.latest_number();
        self.sync_up_to(target).await;
    }

    /// Read the source's safe and finalized head numbers, write the safe head
    /// entry to [`SafeDB`], and issue a consolidated `fork_choice_updated`.
    ///
    /// Mirrors the `update_safe_and_finalized` logic in
    /// `SyncFromSourceTask` from the production follow node.
    pub async fn update_safe_and_finalized(&mut self) {
        let local_tip = self.unsafe_head.block_info.number;
        let clamped_safe = self.source.safe_number().min(local_tip);

        if clamped_safe == 0 {
            return;
        }

        let Some(safe_block) = self.source.get_block(clamped_safe) else {
            return;
        };

        let safe_hash = safe_block.header.hash_slow();

        // Decode L2BlockInfo to extract the l1_origin for the SafeDB key.
        match L2BlockInfo::from_block_and_genesis(&safe_block, &self.rollup_config.genesis) {
            Ok(l2_info) => {
                let l1_block = BlockInfo {
                    number: l2_info.l1_origin.number,
                    hash: l2_info.l1_origin.hash,
                    ..Default::default()
                };
                if let Err(e) = self.safe_db.safe_head_updated(l2_info, l1_block).await {
                    panic!("TestFollowNode: safe_db update failed: {e}");
                }
                self.safe_head = L2BlockInfo {
                    block_info: BlockInfo {
                        hash: safe_hash,
                        number: clamped_safe,
                        parent_hash: safe_block.header.parent_hash,
                        timestamp: safe_block.header.timestamp,
                    },
                    l1_origin: l2_info.l1_origin,
                    seq_num: l2_info.seq_num,
                };
            }
            Err(e) => {
                panic!(
                    "TestFollowNode: failed to decode L2BlockInfo for safe block {clamped_safe}: {e}"
                );
            }
        }

        let clamped_finalized = self.source.finalized_number().min(local_tip);
        if clamped_finalized > 0 {
            if let Some(fin_block) = self.source.get_block(clamped_finalized) {
                let fin_hash = fin_block.header.hash_slow();
                if let Ok(fin_info) =
                    L2BlockInfo::from_block_and_genesis(&fin_block, &self.rollup_config.genesis)
                {
                    self.finalized_head = L2BlockInfo {
                        block_info: BlockInfo {
                            hash: fin_hash,
                            number: clamped_finalized,
                            parent_hash: fin_block.header.parent_hash,
                            timestamp: fin_block.header.timestamp,
                        },
                        l1_origin: fin_info.l1_origin,
                        seq_num: fin_info.seq_num,
                    };
                }
            }
        }

        // Issue a consolidated FCU that sets unsafe/safe/finalized in one call.
        let timestamp = safe_block.header.timestamp;
        let fcu = ForkchoiceState {
            head_block_hash: self.unsafe_head.block_info.hash,
            safe_block_hash: safe_hash,
            finalized_block_hash: if self.finalized_head.block_info.number > 0 {
                self.finalized_head.block_info.hash
            } else {
                B256::ZERO
            },
        };
        match EngineForkchoiceVersion::from_cfg(&self.rollup_config, timestamp) {
            EngineForkchoiceVersion::V2 => {
                self.engine
                    .fork_choice_updated_v2(fcu, None)
                    .await
                    .expect("TestFollowNode: consolidated fcu_v2 failed");
            }
            EngineForkchoiceVersion::V3 => {
                self.engine
                    .fork_choice_updated_v3(fcu, None)
                    .await
                    .expect("TestFollowNode: consolidated fcu_v3 failed");
            }
        }
    }

    /// Return the L1 origin extracted from block `number` in the source bridge.
    ///
    /// Convenience method for test assertions: returns `None` if the block is not
    /// in the source or if the L1 info deposit cannot be decoded.
    pub fn l1_origin_of_block(&self, number: u64) -> Option<BlockNumHash> {
        let block = self.source.get_block(number)?;
        L2BlockInfo::from_block_and_genesis(&block, &self.rollup_config.genesis)
            .ok()
            .map(|info| info.l1_origin)
    }
}
