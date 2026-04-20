//! Action tests for L2 unsafe-head gossip via the production actor stack.

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// The P2P gossip pattern: unsafe chain advances first, then safe catches up.
///
/// 1. A sequencer builds 5 L2 blocks.
/// 2. The node is initialized before any L1 batch is available.
/// 3. Each block is gossiped via [`act_l2_unsafe_gossip_receive`], advancing
///    `unsafe_head` to 5.  `safe_head` stays at genesis throughout because the
///    L1 batch has not landed yet and the L1WatcherActor has nothing to deliver.
/// 4. All blocks are batched and submitted to L1; one L1 block is mined.
/// 5. `sync_until_safe(5)` drives derivation; `safe_head` catches up to
///    `unsafe_head` (both at 5).
///
/// [`act_l2_unsafe_gossip_receive`]: base_action_harness::TestActorDerivationNode::act_l2_unsafe_gossip_receive
#[tokio::test(start_paused = true)]
async fn test_unsafe_chain_advances_safe_catches_up() {
    const L2_BLOCK_COUNT: u64 = 5;

    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut blocks = Vec::with_capacity(L2_BLOCK_COUNT as usize);
    let mut source = ActionL2Source::new();
    for _ in 0..L2_BLOCK_COUNT {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block.clone());
        blocks.push(block);
    }

    // Create the node with the genesis chain only — no L1 batch yet.
    // The L1WatcherActor will see only genesis on its first poll, so derivation
    // cannot run during gossip and safe_head stays at 0.
    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    // Gossip each block sequentially; unsafe_head advances to 5.
    for block in &blocks {
        node.act_l2_unsafe_gossip_receive(block).await;
    }

    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        L2_BLOCK_COUNT,
        "unsafe_head should have advanced to block {L2_BLOCK_COUNT} after gossip"
    );
    assert_eq!(
        node.engine.safe_head().block_info.number,
        0,
        "safe_head should still be at genesis before batches land"
    );

    // Batch and mine; push the new L1 block so the L1WatcherActor picks it up.
    let mut batcher = Batcher::new(source, &h.rollup_config, batcher_cfg.clone());
    batcher.advance(&mut h.l1).await;
    chain.push(h.l1.tip().clone());

    // Drive derivation; safe_head catches up.
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

/// Gossiped blocks arriving out of sequential order are silently dropped.
///
/// The gap guard accepts only `block.number == unsafe_head.number + 1`.
/// Injecting block 3 when `unsafe_head` is at genesis (0) is a gap-jump:
/// 3 ≠ 0 + 1, so the block is dropped and `unsafe_head` stays at 0.
#[tokio::test(start_paused = true)]
async fn test_out_of_order_gossip_is_dropped() {
    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let _block2 = sequencer.build_next_block_with_single_transaction().await;
    let block3 = sequencer.build_next_block_with_single_transaction().await;

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    // Block 3 first — gap-jump; must be dropped.
    node.act_l2_unsafe_gossip_receive(&block3).await;
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        0,
        "unsafe_head must not advance when block 3 arrives before blocks 1 and 2"
    );

    // Block 1 — sequential; must advance.
    node.act_l2_unsafe_gossip_receive(&block1).await;
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        1,
        "unsafe_head should advance to 1 after sequential gossip of block 1"
    );

    // Block 3 again — still a gap (unsafe_head=1, next expected=2); must be dropped.
    node.act_l2_unsafe_gossip_receive(&block3).await;
    assert_eq!(
        node.engine.unsafe_head().block_info.number,
        1,
        "unsafe_head must stay at 1 when block 3 arrives before block 2"
    );
}
