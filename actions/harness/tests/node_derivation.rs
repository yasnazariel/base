//! Action tests exercising end-to-end derivation via the production actor stack.
//!
//! These tests replace the former [`TestRollupNode`]-based derivation tests with
//! [`TestActorDerivationNode`], which runs the real [`DerivationActor`],
//! [`EngineActor`], and [`L1WatcherActor`] on an in-process HTTP engine.
//!
//! [`TestActorDerivationNode`]: base_action_harness::TestActorDerivationNode
//! [`DerivationActor`]: base_consensus_node::DerivationActor
//! [`EngineActor`]: base_consensus_node::EngineActor
//! [`L1WatcherActor`]: base_consensus_node::L1WatcherActor

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// The production actor stack derives L2 blocks from calldata batches.
///
/// 1. A sequencer builds 3 L2 blocks and encodes them into one L1 block.
/// 2. A [`TestActorDerivationNode`] is created and initialized.
/// 3. The [`L1WatcherActor`] picks up the L1 tip on the first tick;
///    the actor stack derives all 3 blocks.
/// 4. `safe_head` advances to block 3.
///
/// [`TestActorDerivationNode`]: base_action_harness::TestActorDerivationNode
/// [`L1WatcherActor`]: base_consensus_node::L1WatcherActor
#[tokio::test(start_paused = true)]
async fn test_actor_derives_batched_blocks() {
    const L2_BLOCK_COUNT: u64 = 3;

    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut source = ActionL2Source::new();
    for _ in 0..L2_BLOCK_COUNT {
        source.push(sequencer.build_next_block_with_single_transaction().await);
    }
    Batcher::new(source, &h.rollup_config, batcher_cfg).advance(&mut h.l1).await;

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;
    node.sync_until_safe(L2_BLOCK_COUNT).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        L2_BLOCK_COUNT,
        "safe head should be at block {L2_BLOCK_COUNT}"
    );
}

/// P2P gossip advances `unsafe_head` before batches land on L1; the actor
/// stack then derives and the `safe_head` catches up.
///
/// 1. A sequencer builds 5 L2 blocks.
/// 2. A [`TestActorDerivationNode`] is created and initialized.
/// 3. Each block is gossiped via [`act_l2_unsafe_gossip_receive`];
///    `unsafe_head` advances to 5 while `safe_head` stays at 0.
/// 4. All blocks are batched into one L1 block.
/// 5. `sync_until_safe(5)` derives all 5 blocks; `safe_head` catches up.
///
/// [`TestActorDerivationNode`]: base_action_harness::TestActorDerivationNode
/// [`act_l2_unsafe_gossip_receive`]: base_action_harness::TestActorDerivationNode::act_l2_unsafe_gossip_receive
#[tokio::test(start_paused = true)]
async fn test_actor_gossip_then_derivation() {
    const L2_BLOCK_COUNT: u64 = 5;

    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    // Build 5 blocks before any batching.
    let mut blocks = Vec::with_capacity(L2_BLOCK_COUNT as usize);
    let mut source = ActionL2Source::new();
    for _ in 0..L2_BLOCK_COUNT {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block.clone());
        blocks.push(block);
    }

    // Create the node with an empty chain (no batches yet).
    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    // Gossip each block sequentially. act_l2_unsafe_gossip_receive ticks
    // internally so the NetworkActor + EngineActor process each payload.
    for block in &blocks {
        node.act_l2_unsafe_gossip_receive(block).await;
    }

    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        L2_BLOCK_COUNT,
        "unsafe_head should be at {L2_BLOCK_COUNT} after gossip"
    );
    assert_eq!(
        node.engine.safe_head().block_info.number,
        0,
        "safe_head must stay at genesis before batches land"
    );

    // Batch and push to the shared chain; the L1WatcherActor picks it up.
    Batcher::new(source, &h.rollup_config, batcher_cfg).advance(&mut h.l1).await;
    chain.push(h.l1.tip().clone());

    node.sync_until_safe(L2_BLOCK_COUNT).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        L2_BLOCK_COUNT,
        "safe_head should have caught up to block {L2_BLOCK_COUNT}"
    );
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        node.engine.safe_head().block_info.number,
        "unsafe_head and safe_head should be equal after safe chain caught up"
    );
}

/// Out-of-order gossip blocks are silently dropped by the gap guard; only
/// sequential blocks advance `unsafe_head`.
///
/// The gap guard accepts only `block.number == unsafe_head.number + 1`.
/// Block 3 arrives when `unsafe_head = 0` (gap-jump: 3 ≠ 0 + 1 → dropped).
/// Block 1 arrives next (sequential: 1 == 0 + 1 → accepted, `unsafe_head = 1`).
/// Block 3 arrives again (gap: 3 ≠ 1 + 1 → dropped, `unsafe_head = 1`).
#[tokio::test(start_paused = true)]
async fn test_actor_out_of_order_gossip_dropped() {
    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let _block2 = sequencer.build_next_block_with_single_transaction().await;
    let block3 = sequencer.build_next_block_with_single_transaction().await;

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    // Block 3 arrives first — gap-jump, must be dropped.
    node.act_l2_unsafe_gossip_receive(&block3).await;
    assert_eq!(node.engine.unsafe_head().block_info.number, 0, "gap-jump block 3 must be dropped");

    // Block 1 — sequential, must advance.
    node.act_l2_unsafe_gossip_receive(&block1).await;
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        1,
        "block 1 should advance unsafe_head to 1"
    );

    // Block 3 again — still a gap (unsafe_head=1, next expected=2).
    node.act_l2_unsafe_gossip_receive(&block3).await;
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        1,
        "block 3 must be dropped when unsafe_head=1"
    );
}
