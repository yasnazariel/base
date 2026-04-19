//! Action tests that exercise [`TestFollowNode`] end-to-end.
//!
//! These tests validate the **execution kernel and SafeDB storage layer** of
//! the follow node: that blocks from a source are correctly re-executed through
//! the production [`OpPayloadBuilder`], that safe-head mappings are recorded in
//! the real redb-backed [`SafeDB`], and that head pointers (unsafe/safe/finalized)
//! advance correctly.
//!
//! # What these tests do NOT cover
//!
//! - The production actor coordination layer ([`DelegateL2DerivationActor`],
//!   [`EngineActor`], channel message ordering, poll-interval timing).
//! - The RPC surface (`optimism_syncStatus`, `optimism_safeHeadAtL1Block`, etc.)
//!   — [`TestFollowNode`] has no HTTP server; SafeDB is queried at the library
//!   level, not through [`RollupRpc`].
//! - Reorg / fork-divergence scenarios.
//!
//! [`OpPayloadBuilder`]: base_execution_payload_builder::OpPayloadBuilder
//! [`DelegateL2DerivationActor`]: base_consensus_service::DelegateL2DerivationActor
//! [`EngineActor`]: base_consensus_service::EngineActor
//! [`RollupRpc`]: base_consensus_rpc::RollupRpc
//! [`TestFollowNode`]: base_action_harness::TestFollowNode
//! [`SafeDB`]: base_consensus_safedb::SafeDB

use base_action_harness::{
    ActionTestHarness, L1MinerConfig, SharedL1Chain, TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// Follow node syncs unsafe blocks from the source and advances its unsafe head.
///
/// 1. A sequencer builds 5 L2 blocks.
/// 2. All blocks are pushed to the follow node's source bridge.
/// 3. `sync_up_to(5)` processes every block through the in-process engine.
/// 4. Unsafe head advances to 5; safe head advances because `safe_number` is
///    set to match latest.
#[tokio::test]
async fn test_follow_node_syncs_unsafe_blocks() {
    const BLOCK_COUNT: u64 = 5;

    let batcher_cfg = base_action_harness::BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..base_action_harness::BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..BLOCK_COUNT {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    // Source's safe head mirrors latest so every synced block is also "safe".
    source.set_safe_number(BLOCK_COUNT);

    follow_node.sync_up_to(BLOCK_COUNT).await;

    assert_eq!(
        follow_node.l2_unsafe_number(),
        BLOCK_COUNT,
        "unsafe head should advance to {BLOCK_COUNT}"
    );
    assert_eq!(
        follow_node.l2_safe_number(),
        BLOCK_COUNT,
        "safe head should advance to {BLOCK_COUNT}"
    );
}

/// Follow node records L1→L2 safe head mappings in SafeDB.
///
/// After syncing N blocks and calling `update_safe_and_finalized`, the SafeDB
/// must contain an entry for the L1 origin of the safe block. A query for that
/// L1 block number returns the correct L2 safe head hash.
#[tokio::test]
async fn test_follow_node_safedb_records_safe_head() {
    const BLOCK_COUNT: u64 = 3;

    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..BLOCK_COUNT {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(BLOCK_COUNT);

    follow_node.sync_up_to(BLOCK_COUNT).await;

    // Determine the L1 origin of the safe block (block BLOCK_COUNT).
    let l1_origin = follow_node
        .l1_origin_of_block(BLOCK_COUNT)
        .expect("L1 origin must be decodable from the safe block");

    // SafeDB must have an entry at that L1 block number.
    let response = follow_node
        .safe_head_at_l1(l1_origin.number)
        .await
        .expect("SafeDB must contain an entry for the safe block's L1 origin");

    assert_eq!(
        response.l1_block.number, l1_origin.number,
        "SafeDB response l1_block.number must match the queried L1 block number"
    );
    assert_eq!(
        response.safe_head.number, BLOCK_COUNT,
        "SafeDB response safe_head must point to L2 block {BLOCK_COUNT}"
    );
    assert_eq!(
        response.safe_head.hash,
        follow_node.l2_safe().block_info.hash,
        "SafeDB response safe_head hash must match the tracked safe head hash"
    );
}

/// Partial sync: follow node syncs only some blocks, then more, in two rounds.
///
/// Validates that `sync_up_to` is idempotent between calls and that the
/// follow node correctly resumes from where it left off.
#[tokio::test]
async fn test_follow_node_incremental_sync() {
    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;

    // Build 3 blocks and sync them.
    for _ in 0..3 {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(3);
    follow_node.sync_up_to(3).await;
    assert_eq!(follow_node.l2_unsafe_number(), 3);

    // Build 2 more blocks and sync to 5.
    for _ in 0..2 {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(5);
    follow_node.sync_up_to(5).await;
    assert_eq!(follow_node.l2_unsafe_number(), 5);
    assert_eq!(follow_node.l2_safe_number(), 5);
}

/// Safe head lags behind unsafe head when the source has a lower safe number.
///
/// The follow node syncs 5 unsafe blocks but the source marks only 3 as safe.
/// After `update_safe_and_finalized`, `l2_safe_number` must be 3 while
/// `l2_unsafe_number` is 5.
#[tokio::test]
async fn test_follow_node_safe_lags_unsafe() {
    const BLOCK_COUNT: u64 = 5;
    const SAFE_COUNT: u64 = 3;

    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..BLOCK_COUNT {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    // Only first 3 blocks are considered safe by the source.
    source.set_safe_number(SAFE_COUNT);

    follow_node.sync_up_to(BLOCK_COUNT).await;

    assert_eq!(follow_node.l2_unsafe_number(), BLOCK_COUNT, "unsafe head must reach {BLOCK_COUNT}");
    assert_eq!(follow_node.l2_safe_number(), SAFE_COUNT, "safe head must stop at {SAFE_COUNT}");
}

/// SafeDB range-scan returns the latest safe head for queries above any recorded L1 origin.
///
/// Querying an L1 block number *higher* than the safe head's recorded L1 origin should
/// return the most recent safe head recorded, not an error. This exercises the
/// SafeDB's "at-or-before" range-scan behavior using `u64::MAX` as an extreme upper bound.
#[tokio::test]
async fn test_follow_node_safedb_range_query_above_latest() {
    const BLOCK_COUNT: u64 = 2;

    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..BLOCK_COUNT {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(BLOCK_COUNT);
    follow_node.sync_up_to(BLOCK_COUNT).await;

    // Query at a very high L1 number — should return the latest safe head.
    let response = follow_node
        .safe_head_at_l1(u64::MAX)
        .await
        .expect("SafeDB must return an entry for L1 block u64::MAX (latest-or-before scan)");

    assert_eq!(
        response.safe_head.number, BLOCK_COUNT,
        "query at u64::MAX must return the most recent safe head"
    );
}

/// Follow node advances its finalized head when the source sets a finalized number.
///
/// 1. Build 5 blocks; mark 5 as safe and 3 as finalized.
/// 2. After `sync_up_to(5)`, `l2_finalized_number()` must equal 3.
/// 3. `l2_safe_number()` must still equal 5 (finalized ≤ safe).
#[tokio::test]
async fn test_follow_node_finalized_head_advances() {
    const BLOCK_COUNT: u64 = 5;
    const FINALIZED_COUNT: u64 = 3;

    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..BLOCK_COUNT {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(BLOCK_COUNT);
    source.set_finalized_number(FINALIZED_COUNT);

    follow_node.sync_up_to(BLOCK_COUNT).await;

    assert_eq!(follow_node.l2_unsafe_number(), BLOCK_COUNT, "unsafe head must reach {BLOCK_COUNT}");
    assert_eq!(follow_node.l2_safe_number(), BLOCK_COUNT, "safe head must reach {BLOCK_COUNT}");
    assert_eq!(
        follow_node.l2_finalized_number(),
        FINALIZED_COUNT,
        "finalized head must advance to {FINALIZED_COUNT}"
    );
}

/// `sync_up_to` stops gracefully when the target exceeds available blocks.
///
/// If the source has only 3 blocks but `sync_up_to(10)` is called, the follow
/// node must stop at 3 without panicking and without advancing beyond what is
/// available.
#[tokio::test]
async fn test_follow_node_sync_stops_at_available() {
    const AVAILABLE: u64 = 3;
    const TARGET: u64 = 10;

    let batcher_cfg = base_action_harness::BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());
    let (mut follow_node, source) = h.create_test_follow_node(&sequencer, chain);

    let mut seq = sequencer;
    for _ in 0..AVAILABLE {
        let block = seq.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(AVAILABLE);

    follow_node.sync_up_to(TARGET).await;

    assert_eq!(
        follow_node.l2_unsafe_number(),
        AVAILABLE,
        "follow node must stop at the last available block ({AVAILABLE}), not panic at {TARGET}"
    );
}
