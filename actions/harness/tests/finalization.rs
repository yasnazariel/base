//! Action tests for L2 finalization via the verifier pipeline.

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// When multiple L2 blocks are submitted in separate L1 inclusion blocks, finalizing the
/// last inclusion block causes ALL previously submitted L2 blocks to become finalized
/// together. The finalized head advances to the highest L2 block whose inclusion block
/// is at or before the finalized L1 number.
#[tokio::test(start_paused = true)]
async fn finalization_advances_with_multiple_l2_blocks_per_epoch() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Build 3 L2 blocks, all referencing L1 epoch 0 (genesis).
    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut blocks = Vec::new();
    for _ in 0..3 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        blocks.push(block);
    }
    // All blocks should reference epoch 0.
    assert_eq!(sequencer.head().l1_origin.number, 0, "all blocks should be in epoch 0");

    // Submit each block in a separate L1 inclusion block (L1#1, L1#2, L1#3).
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in blocks {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    // Finalized head starts at genesis.
    assert_eq!(node.engine.finalized_head().block_info.number, 0);

    node.sync_until_safe(3).await;
    assert_eq!(node.engine.safe_head().block_info.number, 3, "safe head should reach L2 block 3");

    // Signal that L1 block 3 (the last inclusion block) is finalized.
    // All 3 L2 blocks have derived_from in L1 blocks 1-3, all <= 3, so all finalize.
    let l1_block_3 = h.l1.block_info_at(3);
    node.act_l1_finalized_signal(l1_block_3).await;

    assert_eq!(
        node.engine.finalized_head().block_info.number,
        3,
        "all 3 L2 blocks should finalize when L1 block 3 (last inclusion block) is finalized"
    );

    // SafeDB: each batch landed in its own L1 inclusion block (1, 2, 3).
    // Verify every individual L1→L2 mapping.
    for i in 1u64..=3 {
        let safe = node.safe_head_at_l1(i).await.unwrap();
        assert_eq!(safe.safe_head.number, i, "safedb: safe head at L1#{i} should be L2#{i}");
        assert_eq!(safe.l1_block.number, i, "safedb: l1_block at L1#{i} should be {i}");
    }
}

/// L2 finalization advances incrementally as successive L1 inclusion blocks are finalized.
///
/// Produces L2 blocks across two L1 epochs (epoch 0 and epoch 1). Finalizing the
/// last epoch-0 inclusion block advances the finalized head only through the last L2
/// block in epoch 0, leaving the epoch-1 block pending. Finalizing the epoch-1
/// inclusion block then advances it through the remaining block.
#[tokio::test(start_paused = true)]
async fn finalization_advances_incrementally_with_l1_epochs() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Mine L1 block 1 so the sequencer can advance to epoch 1.
    // L1 block_time = 12, so L1 block 1 has timestamp 12.
    // With L2 block_time = 2, L2 blocks 1-5 (ts 2-10) reference epoch 0,
    // and L2 block 6 (ts 12) advances to epoch 1.
    h.mine_l1_blocks(1);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    // Build 6 L2 blocks: blocks 1-5 in epoch 0, block 6 in epoch 1.
    let mut blocks = Vec::new();
    let mut last_epoch_0_number = 0u64;
    for i in 1..=6u64 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        let head = sequencer.head();
        blocks.push(block);
        if head.l1_origin.number == 0 {
            last_epoch_0_number = i;
        }
    }
    assert_eq!(sequencer.head().l1_origin.number, 1, "last L2 block should reference epoch 1");
    assert!(last_epoch_0_number > 0, "at least one L2 block should reference epoch 0");

    // Submit each L2 block in a separate L1 inclusion block.
    // L1#1 is the epoch-providing block (already mined), batches land in L1#2..L1#7.
    // derived_from(L2#i) = L1#(i+1) for i in 1..=6.
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in blocks {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(6).await;
    assert_eq!(node.engine.safe_head().block_info.number, 6, "safe head should reach L2 block 6");

    // First finalization: signal the last epoch-0 inclusion block.
    // L2 blocks 1..last_epoch_0_number have derived_from <= L1#(last_epoch_0_number + 1),
    // so they all finalize. L2 block 6 (derived_from = L1#7) must stay pending.
    let l1_last_epoch0_inclusion = h.l1.block_info_at(last_epoch_0_number + 1);
    node.act_l1_finalized_signal(l1_last_epoch0_inclusion).await;
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        last_epoch_0_number,
        "first signal (last epoch-0 inclusion): only epoch-0 blocks should finalize"
    );
    assert!(
        node.engine.finalized_head().block_info.number < 6,
        "epoch-1 block (L2 block 6) must not yet be finalized"
    );

    // Second finalization: signal the epoch-1 inclusion block. Now block 6 finalizes.
    let l1_epoch1_inclusion = h.l1.block_info_at(last_epoch_0_number + 2);
    node.act_l1_finalized_signal(l1_epoch1_inclusion).await;
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        6,
        "second signal (epoch-1 inclusion): block 6 should now be finalized"
    );
}

/// The finalized L2 head must never exceed the safe head, even when the L1
/// finalized signal references a block far ahead of what has been derived.
#[tokio::test(start_paused = true)]
async fn finalization_does_not_exceed_safe_head() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Build 2 L2 blocks in epoch 0.
    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let block2 = sequencer.build_next_block_with_single_transaction().await;

    // Submit each block via the batcher (L1#1 and L1#2 contain the batches).
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in [block1, block2] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    // Mine many more L1 blocks without any corresponding L2 derivation data.
    h.mine_l1_blocks(10);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "safe head should be 2");

    // Signal an L1 finalized block FAR beyond what's been derived (block 12).
    // The finalizer only knows about L1 blocks 1-2 (inclusion blocks for L2#1 and L2#2),
    // so it can finalize both L2 blocks but cannot go beyond what was derived.
    let l1_far_ahead = h.l1.block_info_at(12);
    node.act_l1_finalized_signal(l1_far_ahead).await;

    assert!(
        node.engine.finalized_head().block_info.number <= node.engine.safe_head().block_info.number,
        "finalized head ({}) must never exceed safe head ({})",
        node.engine.finalized_head().block_info.number,
        node.engine.safe_head().block_info.number,
    );
    // Both L2 blocks have derived_from (L1#1, L1#2) <= 12, so they finalize.
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        2,
        "finalized head should be capped at safe head (2)"
    );
}

/// After a pipeline reset (simulating a reorg), finalization state is cleared
/// back to genesis. After re-deriving blocks, finalization can proceed again.
#[tokio::test(start_paused = true)]
async fn finalization_reorg_clears_state() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    // Build 2 L2 blocks.
    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let block2 = sequencer.build_next_block_with_single_transaction().await;

    // Submit and mine: L2#1 → L1#1, L2#2 → L1#2.
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in [block1, block2] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2);

    // SafeDB pre-reset: L2#2 was derived from L1#2.
    let safe_pre = node.safe_head_at_l1(2).await.unwrap();
    assert_eq!(safe_pre.safe_head.number, 2, "safedb pre-reset: safe head at L1#2 should be L2#2");
    assert_eq!(safe_pre.l1_block.number, 2, "safedb pre-reset: l1_block at L1#2 should be 2");

    // Finalize L1 block 2 (last inclusion block) → both L2 blocks finalize.
    let l1_block_2 = h.l1.block_info_at(2);
    node.act_l1_finalized_signal(l1_block_2).await;
    assert_eq!(node.engine.finalized_head().block_info.number, 2, "pre-reset finalized = 2");

    // Truncate the shared chain to genesis BEFORE resetting so the pipeline
    // sees no old L1 data during the reset's attempt_derivation() call.
    chain.truncate_to(0);

    // Simulate a reorg by resetting the pipeline to genesis.
    let l2_genesis = h.l2_genesis();

    node.act_reset(l2_genesis).await;

    // After reset, finalized head should be back to genesis (block 0).
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        0,
        "finalized head should reset to genesis after pipeline reset"
    );

    // Re-mine a new fork block for the reset pipeline.
    h.l1.reorg_to(0).expect("reorg to genesis");
    // Build a new L2 block on the fresh fork.
    let l1_chain_fresh = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer_fresh = h.create_l2_sequencer(l1_chain_fresh);
    let block1_fresh = sequencer_fresh.build_next_block_with_single_transaction().await;

    // Register the block hash before mining so the node can validate it.
    node.register_block_hash(1, block1_fresh.header.hash_slow());

    let mut source = ActionL2Source::new();
    source.push(block1_fresh);
    let mut batcher = Batcher::new(source, &h.rollup_config, batcher_cfg.clone());
    batcher.advance(&mut h.l1).await;

    // Push the new L1 block containing the fresh batch to the shared chain.
    chain.push(h.l1.tip().clone());

    let l1_block_1_new = h.l1.block_info_at(1);
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "safe head re-derived to 1");

    // SafeDB post-reset: DB was truncated on reset and re-anchored at genesis.
    // After re-deriving L2#1 from new L1#1 the DB reflects the new mapping.
    let safe_post = node.safe_head_at_l1(1).await.unwrap();
    assert_eq!(
        safe_post.safe_head.number, 1,
        "safedb post-reset: safe head at L1#1 should be L2#1"
    );
    assert_eq!(safe_post.l1_block.number, 1, "safedb post-reset: l1_block at L1#1 should be 1");

    // The old L1#2 entry must be gone — querying L1#2 should resolve to the
    // reset anchor (L1#1 → L2#1), not the pre-reset L2#2 entry.
    let safe_l1_2 = node.safe_head_at_l1(2).await.unwrap();
    assert_eq!(
        safe_l1_2.l1_block.number, 1,
        "safedb post-reset: L1#2 query must resolve to reset anchor at L1#1, not stale pre-reset entry"
    );
    assert_eq!(
        safe_l1_2.safe_head.number, 1,
        "safedb post-reset: L1#2 query must return reset L2#1, not stale pre-reset L2#2"
    );

    // Finalize the new L1 block 1 → finalization works again.
    node.act_l1_finalized_signal(l1_block_1_new).await;
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        1,
        "finalization should work cleanly after reset and re-derivation"
    );
}

/// Once L2 finalization reaches block N, signalling an older L1 block as
/// finalized must not regress the finalized L2 head.
#[tokio::test(start_paused = true)]
async fn finalization_does_not_regress() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Mine L1 block 1 for epoch advancement.
    h.mine_l1_blocks(1);

    // Build 6 L2 blocks: blocks 1-5 in epoch 0, block 6 in epoch 1.
    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut blocks = Vec::new();
    for _ in 0..6 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        blocks.push(block);
    }

    // Submit each L2 block in a separate L1 inclusion block.
    // Batches land in L1#2..L1#7 (L1#1 is the epoch-providing block).
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in blocks {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(6).await;
    assert_eq!(node.engine.safe_head().block_info.number, 6, "safe head should be 6");

    // Finalize L1 block 7 (last inclusion block). All 6 L2 blocks have
    // derived_from in L1#2..L1#7, all <= 7, so all 6 finalize.
    let l1_block_7 = h.l1.block_info_at(7);
    node.act_l1_finalized_signal(l1_block_7).await;
    let finalized_after_first = node.engine.finalized_head().block_info.number;
    assert_eq!(finalized_after_first, 6, "all 6 blocks should be finalized");

    // Now signal an OLDER L1 block (genesis, block 0) as finalized.
    // This should NOT cause the finalized head to regress.
    let l1_genesis = h.l1.block_info_at(0);
    node.act_l1_finalized_signal(l1_genesis).await;

    assert_eq!(
        node.engine.finalized_head().block_info.number,
        finalized_after_first,
        "finalized head must not regress when an older L1 block is signalled as finalized"
    );
}
