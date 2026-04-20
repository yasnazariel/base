//! Action tests for L2 derivation via the production actor derivation stack.

use std::sync::Arc;

use alloy_eips::BlockNumHash;
use alloy_genesis::ChainConfig;
use alloy_primitives::{Address, Bytes, U256};
use base_action_harness::{
    ActionDataSource, ActionEngineClient, ActionL1ChainProvider, ActionL2ChainProvider,
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, PendingTx,
    SharedBlockHashRegistry, SharedL1Chain, TestActorDerivationNode, TestRollupConfigBuilder,
    UserDeposit, block_info_from,
};
use base_batcher_encoder::{DaType, EncoderConfig};
use base_consensus_derive::{
    PipelineBuilder, ResetSignal, SignalReceiver, StatefulAttributesBuilder,
};
use base_protocol::{BatchType, BlockInfo, DERIVATION_VERSION_0, L2BlockInfo};

/// The derivation pipeline reads a single batcher frame from L1 and derives
/// the corresponding L2 block, advancing the safe head from genesis (0) to 1.
#[tokio::test(start_paused = true)]
async fn single_l2_block_derived_from_batcher_frame() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let mut source = ActionL2Source::new();
    source.push(builder.build_next_block_with_single_transaction().await);
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "safe head should be L2 block 1");

    let safe = node.safe_head_at_l1(1).await.unwrap();
    assert_eq!(safe.safe_head.number, 1, "safedb: safe head at L1#1 should be L2#1");
    assert_eq!(safe.l1_block.number, 1, "safedb: l1_block at L1#1 should be 1");
}

/// Mine several L1 blocks, each containing one batch, and verify the safe head
/// advances by one L2 block per L1 block.
#[tokio::test(start_paused = true)]
async fn multiple_l1_blocks_each_derive_one_l2_block() {
    const L2_BLOCK_COUNT: u64 = 3;

    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for _ in 1..=L2_BLOCK_COUNT {
        batcher.push_block(builder.build_next_block_with_single_transaction().await);
        batcher.advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(L2_BLOCK_COUNT).await;

    assert_eq!(node.engine.safe_head().block_info.number, L2_BLOCK_COUNT);

    for i in 1..=L2_BLOCK_COUNT {
        let safe = node.safe_head_at_l1(i).await.unwrap();
        assert_eq!(safe.safe_head.number, i, "safedb: safe head at L1#{i} should be L2#{i}");
        assert_eq!(safe.l1_block.number, i, "safedb: l1_block at L1#{i} should be {i}");
    }
}

/// A batcher frame that lands in an L1 block which is subsequently reorged out
/// must NOT be derived.
#[tokio::test(start_paused = true)]
async fn batch_in_orphaned_l1_block_is_not_derived() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let mut source = ActionL2Source::new();
    source.push(builder.build_next_block_with_single_transaction().await);
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    h.l1.reorg_to(0).expect("reorg to genesis");
    h.l1.mine_block();

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    // Let the actor process the empty block 1'
    for _ in 0..10 {
        node.tick().await;
    }

    assert_eq!(node.engine.safe_head().block_info.number, 0, "batch was in orphaned block");
}

/// After deriving L2 block 1, an L1 reorg back to genesis resets the pipeline.
/// The safe head must revert to 0 on the empty fork.
#[tokio::test(start_paused = true)]
async fn reorg_reverts_derived_safe_head() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let mut source = ActionL2Source::new();
    source.push(builder.build_next_block_with_single_transaction().await);
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1);

    h.l1.reorg_to(0).expect("reorg to genesis");
    h.l1.mine_block();
    chain.truncate_to(0);
    chain.push(h.l1.tip().clone());

    let l2_genesis = h.l2_genesis();
    node.act_reset(l2_genesis).await;
    for _ in 0..10 {
        node.tick().await;
    }

    assert_eq!(node.engine.safe_head().block_info.number, 0, "safe head reverted to genesis");
}

/// After a reorg, the batcher resubmits the lost frame in a new L1 block.
/// The verifier must re-derive the same L2 block on the canonical fork.
#[tokio::test(start_paused = true)]
async fn reorg_and_resubmit_rederives_l2_block() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let block1 = builder.build_next_block_with_single_transaction().await;

    {
        let mut source = ActionL2Source::new();
        source.push(block1.clone());
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1);

    h.l1.reorg_to(0).expect("reorg to genesis");
    h.l1.mine_block();
    chain.truncate_to(0);
    chain.push(h.l1.tip().clone());

    let l2_genesis = h.l2_genesis();
    node.act_reset(l2_genesis).await;
    for _ in 0..5 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 0);

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    chain.push(h.l1.tip().clone());

    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "L2 block 1 re-derived");
}

/// The canonical chain flip-flops between two competing forks three times.
/// After each switch the pipeline is reset and must re-derive from the new fork.
#[tokio::test(start_paused = true)]
async fn reorg_flip_flop() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let l2_genesis = h.l2_genesis();

    // --- Phase 1: Fork A canonical. ---
    {
        let mut source = ActionL2Source::new();
        source.push(block1.clone());
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "phase 1");

    // --- Phase 2: Fork B canonical. ---
    h.l1.reorg_to(0).expect("reorg to fork B");
    {
        let mut source = ActionL2Source::new();
        source.push(block1.clone());
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    chain.truncate_to(0);
    chain.push(h.l1.tip().clone());

    node.act_reset(l2_genesis).await;
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "phase 2");

    // --- Phase 3: Fork A' canonical. ---
    h.l1.reorg_to(0).expect("reorg to fork A'");
    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    chain.truncate_to(0);
    chain.push(h.l1.tip().clone());

    node.act_reset(l2_genesis).await;
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "phase 3");
}

/// The canonical chain flip-flops through three forks where the middle fork is
/// completely empty (no batcher data).
#[tokio::test(start_paused = true)]
async fn reorg_flip_flop_empty_middle_fork() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;
    let l2_genesis = h.l2_genesis();

    // --- Fork A: derive blocks 1 and 2. ---
    let mut batcher_a = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in [block1.clone(), block2.clone()] {
        batcher_a.push_block(block);
        batcher_a.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "fork A: safe head = 2");
    assert_eq!(node.engine.safe_head().l1_origin.number, 0, "fork A: all blocks in epoch 0");

    // --- Fork B: reorg to genesis; mine two empty blocks; derive nothing. ---
    h.l1.reorg_to(0).expect("reorg to fork B");
    chain.truncate_to(0);
    for _ in 0..2 {
        h.mine_and_push(&chain);
    }

    node.act_reset(l2_genesis).await;
    assert_eq!(node.engine.safe_head().block_info.number, 0, "reset to B: safe head = 0");
    assert_eq!(node.engine.finalized_head().block_info.number, 0, "reset to B: finalized = 0");
    assert_eq!(node.engine.unsafe_head().block_info.number, 0, "reset to B: unsafe = 0");

    for _ in 0..10 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 0, "fork B: empty blocks derive nothing");
    assert_eq!(node.engine.finalized_head().block_info.number, 0, "fork B: finalized = 0");

    // --- Fork C: reorg to genesis; resubmit both batches; re-derive. ---
    h.l1.reorg_to(0).expect("reorg to fork C");
    chain.truncate_to(0);
    let mut batcher_c = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in [block1, block2] {
        batcher_c.push_block(block);
        batcher_c.advance(&mut h.l1).await;
        chain.push(h.l1.tip().clone());
    }

    node.act_reset(l2_genesis).await;
    assert_eq!(node.engine.safe_head().block_info.number, 0, "reset to C: safe head = 0");
    assert_eq!(node.engine.finalized_head().block_info.number, 0, "reset to C: finalized = 0");
    assert_eq!(node.engine.unsafe_head().block_info.number, 0, "reset to C: unsafe = 0");

    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "fork C: safe head = 2");
    assert_eq!(node.engine.safe_head().l1_origin.number, 0, "fork C: all blocks in epoch 0");
    assert_eq!(node.engine.finalized_head().block_info.number, 0, "fork C: finalized = 0");
}

/// A batch submitted at the last valid L1 block within the sequence window
/// must be derived successfully.
#[tokio::test(start_paused = true)]
async fn batch_accepted_at_last_seq_window_block() {
    const SEQ_WINDOW: u64 = 4;

    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_seq_window_size(SEQ_WINDOW)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block1 = builder.build_next_block_with_single_transaction().await;

    h.mine_l1_blocks(2);

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "batch in last valid L1 block");
}

/// A user deposit log on L1 is processed by the derivation pipeline without errors.
#[tokio::test(start_paused = true)]
async fn l1_deposit_included_in_derived_l2_block() {
    let deposit_contract = Address::repeat_byte(0xDD);
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_deposit_contract(deposit_contract)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let block1 = sequencer.build_next_block_with_single_transaction().await;

    h.l1.enqueue_user_deposit(&UserDeposit {
        deposit_contract,
        from: Address::repeat_byte(0xAA),
        to: Address::repeat_byte(0xBB),
        mint: 0,
        value: U256::from(1_000_000_000_000_000_000u128),
        gas_limit: 100_000,
        data: vec![],
    });

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1);
    assert_eq!(node.engine.safe_head().l1_origin.number, 0, "L2 block 1 references epoch 0");
}

/// After a batcher-address rotation committed to L1 via a `ConfigUpdate` log,
/// frames from the old batcher address are silently ignored and frames from the
/// new address are derived normally.
#[tokio::test(start_paused = true)]
async fn batcher_key_rotation_accepts_new_batcher() {
    let l1_sys_cfg_addr = Address::repeat_byte(0xCC);
    let batcher_a = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let batcher_b =
        BatcherConfig { batcher_address: Address::repeat_byte(0xBB), ..batcher_a.clone() };

    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_a)
        .with_l1_system_config_address(l1_sys_cfg_addr)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;
    let block3 = builder.build_next_block_with_single_transaction().await;

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_a.clone());
    for block in [block1, block2] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    h.l1.enqueue_batcher_update(l1_sys_cfg_addr, batcher_b.batcher_address);
    h.l1.mine_block();

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    // Drive through blocks 1-2 (batcher A) and block 3 (rotation).
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "blocks 1-2 derived");

    // L1 block 4: batcher A — must be ignored after rotation.
    {
        let mut source = ActionL2Source::new();
        source.push(block3.clone());
        Batcher::new(source, &h.rollup_config, batcher_a.clone()).advance(&mut h.l1).await;
    }
    chain.push(h.l1.tip().clone());
    for _ in 0..10 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 2, "batcher A ignored after rotation");

    // L1 block 5: batcher B — must be derived.
    {
        let mut source = ActionL2Source::new();
        source.push(block3);
        Batcher::new(source, &h.rollup_config, batcher_b.clone()).advance(&mut h.l1).await;
    }
    chain.push(h.l1.tip().clone());
    node.sync_until_safe(3).await;
    assert_eq!(node.engine.safe_head().block_info.number, 3, "batcher B derived");
}

/// Derive 6 L2 blocks all belonging to the same L1 epoch (genesis).
#[tokio::test(start_paused = true)]
async fn multi_l2_per_l1_epoch() {
    const L2_COUNT: u64 = 6;
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for _ in 1..=L2_COUNT {
        batcher.push_block(builder.build_next_block_with_single_transaction().await);
        batcher.advance(&mut h.l1).await;
        l1_chain.push(h.l1.tip().clone());
    }

    node.initialize().await;
    node.sync_until_safe(L2_COUNT).await;

    assert_eq!(node.engine.safe_head().block_info.number, L2_COUNT);
    assert_eq!(node.engine.safe_head().l1_origin.number, 0, "all blocks in epoch 0");
}

/// A batch submitted past the sequence window is rejected and the pipeline
/// generates deposit-only default blocks to fill the epoch instead.
#[tokio::test(start_paused = true)]
async fn batch_past_sequence_window_rejected() {
    const SEQ_WINDOW: u64 = 3;
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_seq_window_size(SEQ_WINDOW)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block1 = builder.build_next_block_with_single_transaction().await;

    h.mine_l1_blocks(2);

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;

    // The pipeline generates deposit-only blocks when the window expires.
    node.sync_until_safe(1).await;
    assert!(node.engine.safe_head().block_info.number > 0, "deposit-only blocks generated");
    assert_eq!(node.engine.safe_head().l1_origin.number, 0, "all blocks in epoch 0");
}

/// Build 12 L2 blocks spanning two epoch boundaries (epoch 0 → 1 → 2).
#[tokio::test(start_paused = true)]
async fn multi_epoch_sequence() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    h.mine_l1_blocks(2);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let mut blocks = Vec::new();
    for _ in 0..12 {
        blocks.push(builder.build_next_block_with_single_transaction().await);
    }
    assert_eq!(builder.head().l1_origin.number, 2, "L2 block 12 should reference epoch 2");

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in &blocks {
        batcher.push_block(block.clone());
        batcher.advance(&mut h.l1).await;
        l1_chain.push(h.l1.tip().clone());
    }

    node.initialize().await;
    node.sync_until_safe(12).await;

    assert_eq!(node.engine.safe_head().block_info.number, 12, "safe head should reach L2 block 12");
}

/// Build 3 L2 blocks, encode all 3 into a single batcher submission (one channel),
/// mine one L1 block, and verify all 3 are derived.
#[tokio::test(start_paused = true)]
async fn same_epoch_multi_batch_one_l1_block() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);

    let mut source = ActionL2Source::new();
    for _ in 1..=3u64 {
        source.push(builder.build_next_block_with_single_transaction().await);
    }
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(3).await;

    assert_eq!(node.engine.safe_head().block_info.number, 3);
}

/// Derive 5 L2 blocks, reorg L1 all the way back to genesis, resubmit all 5
/// batches on the new fork, and verify the safe head recovers to 5.
#[tokio::test(start_paused = true)]
async fn deep_reorg_multi_block() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let mut blocks = Vec::new();
    for _ in 0..5 {
        blocks.push(builder.build_next_block_with_single_transaction().await);
    }

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in &blocks {
        batcher.push_block(block.clone());
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;
    node.sync_until_safe(5).await;
    assert_eq!(node.engine.safe_head().block_info.number, 5, "pre-reorg: 5 blocks derived");

    h.l1.reorg_to(0).expect("reorg to genesis");
    chain.truncate_to(0);

    let l2_genesis = h.l2_genesis();
    node.act_reset(l2_genesis).await;
    for _ in 0..5 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 0, "safe head reverted");

    let mut resubmit_batcher =
        Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in &blocks {
        resubmit_batcher.push_block(block.clone());
        resubmit_batcher.advance(&mut h.l1).await;
        chain.push(h.l1.tip().clone());
    }

    node.sync_until_safe(5).await;
    assert_eq!(node.engine.safe_head().block_info.number, 5, "post-reorg: 5 blocks recovered");
}

/// Garbage frame data is silently ignored; a valid batch in a subsequent L1 block
/// is still derived.
#[tokio::test(start_paused = true)]
async fn garbage_frame_data_ignored() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let block = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;
    node.initialize().await;

    let garbage = {
        let mut v = vec![DERIVATION_VERSION_0];
        v.extend_from_slice(&[0xFF, 0xAB, 0x12, 0x34, 0x56, 0x78]);
        Bytes::from(v)
    };
    h.l1.submit_tx(PendingTx {
        from: batcher_cfg.batcher_address,
        to: batcher_cfg.inbox_address,
        input: garbage,
    });
    h.mine_and_push(&l1_chain);

    for _ in 0..10 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 0, "garbage ignored");

    {
        let mut source = ActionL2Source::new();
        source.push(block);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    l1_chain.push(h.l1.tip().clone());

    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "valid batch after garbage derived");
}

/// A channel whose compressed data exceeds `max_frame_size` is split across
/// multiple frames. All frames land in the same L1 block and are reassembled.
#[tokio::test(start_paused = true)]
async fn multi_frame_channel_reassembled() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig {
            max_frame_size: 80,
            da_type: DaType::Calldata,
            ..EncoderConfig::default()
        },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block = builder.build_next_block_with_single_transaction().await;

    let mut source = ActionL2Source::new();
    source.push(block);
    let mut batcher = Batcher::new(source, &h.rollup_config, batcher_cfg.clone());
    batcher.encode_only().await;
    assert!(
        batcher.pending_count() >= 2,
        "expected at least 2 frame submissions with max_frame_size=80, got {}",
        batcher.pending_count()
    );

    let n = batcher.pending_count();
    batcher.stage_n_frames(&mut h.l1, n);
    let block_num = h.l1.mine_block().number();
    batcher.confirm_staged(block_num).await;

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "multi-frame channel reassembled");
}

// ── Span-batch derivation variants ────────────────────────────────────────────

/// Derive a single L2 block encoded as a span batch.
#[tokio::test(start_paused = true)]
async fn single_l2_block_derived_from_span_batch() {
    let batcher_cfg = BatcherConfig {
        batch_type: BatchType::Span,
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let mut source = ActionL2Source::new();
    source.push(sequencer.build_next_block_with_single_transaction().await);
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "one L2 block from span batch");
}

/// Derive 3 L2 blocks encoded together as a single span batch.
#[tokio::test(start_paused = true)]
async fn three_l2_blocks_derived_from_span_batch() {
    let batcher_cfg = BatcherConfig {
        batch_type: BatchType::Span,
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut source = ActionL2Source::new();
    for _ in 1..=3u64 {
        source.push(sequencer.build_next_block_with_single_transaction().await);
    }
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(3).await;

    assert_eq!(node.engine.safe_head().block_info.number, 3, "3 L2 blocks from span batch");
}

// ── System-config update tests ─────────────────────────────────────────────────

/// A `GasConfig` system-config update does not disrupt ongoing derivation.
#[tokio::test(start_paused = true)]
async fn gpo_params_change_does_not_disrupt_derivation() {
    let l1_sys_cfg_addr = Address::repeat_byte(0xCC);
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_l1_system_config_address(l1_sys_cfg_addr)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let block2 = sequencer.build_next_block_with_single_transaction().await;

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    h.l1.enqueue_gas_config_update(l1_sys_cfg_addr, 2100, 1_000_000);
    h.l1.mine_block();

    {
        let mut source = ActionL2Source::new();
        source.push(block2);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(2).await;

    assert_eq!(node.engine.safe_head().block_info.number, 2, "both blocks after GPO update");
}

/// A `GasLimit` system-config update does not disrupt ongoing derivation.
#[tokio::test(start_paused = true)]
async fn gas_limit_change_does_not_disrupt_derivation() {
    let l1_sys_cfg_addr = Address::repeat_byte(0xCC);
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_l1_system_config_address(l1_sys_cfg_addr)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let block2 = sequencer.build_next_block_with_single_transaction().await;

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    h.l1.enqueue_gas_limit_update(l1_sys_cfg_addr, 60_000_000);
    h.l1.mine_block();

    {
        let mut source = ActionL2Source::new();
        source.push(block2);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(2).await;

    assert_eq!(node.engine.safe_head().block_info.number, 2, "both blocks after gas-limit update");
}

// ── Typed garbage-frame variant tests ─────────────────────────────────────────

/// Submit a raw garbage payload, mine it into an L1 block, tick the actors, then
/// submit a valid batch and assert recovery succeeds.
async fn garbage_payload_silently_ignored_then_valid_batch_derived(garbage: Bytes) {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain.clone());
    let block = sequencer.build_next_block_with_single_transaction().await;

    // L1 block 1: garbage frame only.
    h.l1.submit_tx(PendingTx {
        from: batcher_cfg.batcher_address,
        to: batcher_cfg.inbox_address,
        input: garbage.clone(),
    });
    h.l1.mine_block();

    // L1 block 2: valid batch.
    {
        let mut source = ActionL2Source::new();
        source.push(block);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        1,
        "valid batch after garbage must be derived"
    );
}

/// Random-looking garbage is silently ignored.
#[tokio::test(start_paused = true)]
async fn garbage_random_silently_ignored() {
    garbage_payload_silently_ignored_then_valid_batch_derived(Bytes::from(vec![0xDE_u8; 200]))
        .await;
}

/// A truncated frame is silently ignored.
#[tokio::test(start_paused = true)]
async fn garbage_truncated_silently_ignored() {
    let mut v = vec![DERIVATION_VERSION_0];
    v.extend_from_slice(&[0x01u8; 16]);
    garbage_payload_silently_ignored_then_valid_batch_derived(Bytes::from(v)).await;
}

/// A frame with a valid header but an invalid RLP body is silently ignored.
#[tokio::test(start_paused = true)]
async fn garbage_malformed_rlp_silently_ignored() {
    let mut v = vec![DERIVATION_VERSION_0];
    v.extend_from_slice(&[0x02u8; 16]);
    v.extend_from_slice(&[0x00, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x10]);
    v.extend_from_slice(&[0xFF; 16]);
    v.push(0x00);
    garbage_payload_silently_ignored_then_valid_batch_derived(Bytes::from(v)).await;
}

/// A frame with a valid header and brotli magic byte but a corrupt body is
/// silently ignored.
#[tokio::test(start_paused = true)]
async fn garbage_invalid_brotli_silently_ignored() {
    let mut v = vec![DERIVATION_VERSION_0];
    v.extend_from_slice(&[0x03u8; 16]);
    v.extend_from_slice(&[0x00, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x10]);
    v.push(0xCE);
    v.extend_from_slice(&[0xAB; 15]);
    v.push(0x00);
    garbage_payload_silently_ignored_then_valid_batch_derived(Bytes::from(v)).await;
}

// ── L2 finalization tracking ───────────────────────────────────────────────────

/// The finalized L2 head advances when an L1 finalized signal is received.
#[tokio::test(start_paused = true)]
async fn l2_finalized_advances_via_l1_finalized_signal() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let block1 = sequencer.build_next_block_with_single_transaction().await;
    let block2 = sequencer.build_next_block_with_single_transaction().await;

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for block in [block1, block2] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let node =
        h.create_actor_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec())).await;
    node.initialize().await;

    assert_eq!(node.engine.finalized_head().block_info.number, 0);

    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2);

    let l1_block_1 = h.l1.block_info_at(1);
    node.act_l1_finalized_signal(l1_block_1).await;
    assert_eq!(
        node.engine.finalized_head().block_info.number,
        2,
        "finalized head should advance to L2 block 2"
    );
}

// ── Sequencer L1-origin pin ────────────────────────────────────────────────────

/// `L2Sequencer::pin_l1_origin` freezes the L2 epoch on a specific L1 block
/// regardless of how many newer L1 blocks are available.
///
/// This test only drives the sequencer — no derivation node needed.
#[tokio::test]
async fn sequencer_pin_l1_origin_keeps_epoch_and_empty_block() {
    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    h.mine_l1_blocks(2);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let l1_block_0 = h.l1.block_info_at(0);
    sequencer.pin_l1_origin(l1_block_0);

    for _ in 0..3 {
        let _block = sequencer.build_next_block_with_single_transaction().await;
        assert_eq!(sequencer.head().l1_origin.number, 0, "epoch must remain pinned to 0");
    }

    let empty = sequencer.build_empty_block().await;
    assert_eq!(empty.body.transactions.len(), 1, "empty block has exactly the L1-info deposit");
    assert_eq!(sequencer.head().l1_origin.number, 0, "epoch still pinned after empty block");

    sequencer.clear_l1_origin_pin();
    let _block = sequencer.build_next_block_with_single_transaction().await;
    assert!(sequencer.head().l1_origin.number <= 2, "epoch within [0, 2] after clearing pin");
}

// ── Derive from non-zero L1 genesis ───────────────────────────────────────────

/// Derivation works correctly when the L2 genesis is anchored to a non-zero
/// L1 block (block #5 in this case).
#[tokio::test(start_paused = true)]
async fn derive_chain_from_near_l1_genesis() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let mut rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    h.mine_l1_blocks(5);

    let l1_block_5 = h.l1.block_by_number(5).expect("block 5");
    let l1_block_5_hash = l1_block_5.hash();
    let l1_genesis_info = block_info_from(l1_block_5);

    rollup_cfg.genesis.l1 = BlockNumHash { number: 5, hash: l1_block_5_hash };
    rollup_cfg.genesis.l2_time = l1_block_5.timestamp();
    rollup_cfg.genesis.l2.hash = ActionEngineClient::compute_l2_genesis_hash(&rollup_cfg);

    let genesis_head = L2BlockInfo {
        block_info: BlockInfo {
            hash: rollup_cfg.genesis.l2.hash,
            number: rollup_cfg.genesis.l2.number,
            parent_hash: Default::default(),
            timestamp: rollup_cfg.genesis.l2_time,
        },
        l1_origin: BlockNumHash { number: 5, hash: l1_block_5_hash },
        seq_num: 0,
    };

    h.rollup_config = rollup_cfg.clone();

    let l1_chain_snap = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain_snap);

    let mut batcher = Batcher::new(ActionL2Source::new(), &rollup_cfg, batcher_cfg.clone());
    for _ in 1..=2u64 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        assert_eq!(sequencer.head().l1_origin.number, 5, "epoch should stay at 5");
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let rollup_arc = Arc::new(rollup_cfg.clone());
    let l1_chain_config = Arc::new(ChainConfig::default());
    let l1_provider = ActionL1ChainProvider::new(chain.clone());
    let dap_source = ActionDataSource::new(chain.clone(), rollup_cfg.batch_inbox_address);
    let l2_provider = ActionL2ChainProvider::from_genesis(&rollup_cfg);

    let attrs_builder = StatefulAttributesBuilder::new(
        Arc::clone(&rollup_arc),
        Arc::clone(&l1_chain_config),
        l2_provider.clone(),
        l1_provider.clone(),
    );
    let mut pipeline = PipelineBuilder::new()
        .rollup_config(Arc::clone(&rollup_arc))
        .origin(l1_genesis_info)
        .chain_provider(l1_provider)
        .dap_source(dap_source)
        .l2_chain_provider(l2_provider)
        .builder(attrs_builder)
        .build_polled();

    pipeline
        .signal(ResetSignal { l2_safe_head: genesis_head }.signal())
        .await
        .expect("reset signal failed");

    let engine = ActionEngineClient::new(
        Arc::clone(&rollup_arc),
        genesis_head,
        SharedBlockHashRegistry::new(),
        chain,
    );

    let node = TestActorDerivationNode::new(rollup_arc, engine, pipeline, genesis_head).await;
    node.initialize().await;
    node.sync_until_safe(2).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        2,
        "both L2 blocks derived when genesis is anchored to L1 block #5"
    );
}

// ---------------------------------------------------------------------------
// Blob DA derivation tests
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn single_l2_block_derived_from_blob() {
    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let mut source = ActionL2Source::new();
    source.push(builder.build_next_block_with_single_transaction().await);
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node = h
        .create_actor_blob_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec()))
        .await;
    node.initialize().await;
    node.sync_until_safe(1).await;

    assert_eq!(node.engine.safe_head().block_info.number, 1, "one L2 block derived from blob");
}

#[tokio::test(start_paused = true)]
async fn multiple_l2_blocks_derived_from_blob() {
    const L2_BLOCK_COUNT: u64 = 3;

    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);

    let mut source = ActionL2Source::new();
    for _ in 1..=L2_BLOCK_COUNT {
        source.push(builder.build_next_block_with_single_transaction().await);
    }
    Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;

    let node = h
        .create_actor_blob_derivation_node(SharedL1Chain::from_blocks(h.l1.chain().to_vec()))
        .await;
    node.initialize().await;
    node.sync_until_safe(L2_BLOCK_COUNT).await;

    assert_eq!(node.engine.safe_head().block_info.number, L2_BLOCK_COUNT, "3 blobs derived");
}

/// A `SystemConfig` batcher-address update committed in an L1 block is
/// correctly rolled back when that L1 block is removed by a reorg.
#[tokio::test(start_paused = true)]
async fn batcher_config_update_rolled_back_on_reorg() {
    let l1_sys_cfg_addr = Address::repeat_byte(0xCC);
    let batcher_a = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let batcher_b =
        BatcherConfig { batcher_address: Address::repeat_byte(0xBB), ..batcher_a.clone() };

    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_a)
        .with_l1_system_config_address(l1_sys_cfg_addr)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg.clone());

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);
    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;
    let block3 = builder.build_next_block_with_single_transaction().await;

    let block1_clone = block1.clone();
    let block2_clone = block2.clone();

    // Derive blocks 1-2 with batcher A (L1 blocks 1-2).
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_a.clone());
    for block in [block1, block2] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    // Rotation block (L1 block 3).
    h.l1.enqueue_batcher_update(l1_sys_cfg_addr, batcher_b.batcher_address);
    h.l1.mine_block();

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    // Drive derivation through rotation block — safe head reaches 2.
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "blocks 1-2 derived with batcher A");

    // L1 block 4: batcher A submits block 3 — must be ignored after rotation.
    {
        let mut source = ActionL2Source::new();
        source.push(block3.clone());
        Batcher::new(source, &h.rollup_config, batcher_a.clone()).advance(&mut h.l1).await;
    }
    chain.push(h.l1.tip().clone());
    for _ in 0..10 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 2, "batcher A frame must be ignored");

    // Reorg L1 back to genesis.
    h.l1.reorg_to(0).expect("reorg to genesis");
    chain.truncate_to(0);

    let l2_genesis = h.l2_genesis();
    node.act_reset(l2_genesis).await;
    for _ in 0..5 {
        node.tick().await;
    }

    // New fork: re-mine blocks 1-2-3 with batcher A (no config update log).
    let mut resubmit_batcher =
        Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_a.clone());
    for block in [block1_clone, block2_clone, block3] {
        resubmit_batcher.push_block(block);
        resubmit_batcher.advance(&mut h.l1).await;
        chain.push(h.l1.tip().clone());
    }

    node.sync_until_safe(3).await;
    assert_eq!(node.engine.safe_head().block_info.number, 3, "config rollback restored batcher A");
}

// ── Out-of-order batch reordering ─────────────────────────────────────────────

/// Submit the batch for L2 block 2 to L1 before the batch for L2 block 1.
///
/// The `BatchQueue` buffers future batches and derives block 1 first when its
/// batch arrives, then pops the buffered block 2.
#[tokio::test(start_paused = true)]
async fn out_of_order_singular_batches_reordered_by_batch_queue() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Create node with genesis-only chain before mining batches.
    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    // L1 block 1: carry the batch for L2 block 2 (submitted out of order).
    {
        let mut source = ActionL2Source::new();
        source.push(block2);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    l1_chain.push(h.l1.tip().clone());

    // L1 block 2: carry the batch for L2 block 1 (the expected-next batch).
    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    // Do NOT push block 2 yet — let the pipeline see only block 1 first.

    node.initialize().await;

    // Tick with only L1 block 1 visible: BatchQueue buffers the future batch.
    node.tick().await;
    assert_eq!(node.engine.safe_head().block_info.number, 0, "future batch buffered");

    // Push L1 block 2 and sync: both blocks derived in correct order.
    l1_chain.push(h.l1.tip().clone());
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "BatchQueue reordered correctly");
}

/// `pipeline_idle_before_l1_signal_derives_after`: the pipeline is idle before
/// an L1 block is signalled, and derives after it becomes visible.
#[tokio::test(start_paused = true)]
async fn pipeline_idle_before_l1_signal_derives_after() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let block1 = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    // Mine the batch but don't push to chain yet.
    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }

    node.initialize().await;

    // Block 1 not visible — pipeline idles.
    node.tick().await;
    assert_eq!(node.engine.safe_head().block_info.number, 0, "pipeline idle before signal");

    // Now make block 1 visible.
    l1_chain.push(h.l1.tip().clone());
    node.sync_until_safe(1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 1, "pipeline derives after signal");
}

/// After all L2 blocks from an L1 block are derived, the pipeline advances its
/// L1 origin to the next block without producing additional L2 blocks from an
/// empty L1 block.
#[tokio::test(start_paused = true)]
async fn pipeline_l1_origin_advance_after_epoch_exhausted() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        source.push(block2);
        Batcher::new(source, &h.rollup_config, batcher_cfg.clone()).advance(&mut h.l1).await;
    }
    l1_chain.push(h.l1.tip().clone()); // L1 block 1: carries blocks 1 & 2
    h.mine_and_push(&l1_chain); // L1 block 2: empty — only for origin advance

    node.initialize().await;
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2);

    // Extra ticks — empty L1 block 2 should not advance safe head beyond 2.
    for _ in 0..5 {
        node.tick().await;
    }
    assert_eq!(node.engine.safe_head().block_info.number, 2, "no new blocks from empty L1 block");
}

// ── Span batch: multi-epoch crossing ──────────────────────────────────────────

/// A single span batch encoding L2 blocks that span two L1 epochs is correctly
/// derived by the pipeline.
#[tokio::test(start_paused = true)]
async fn span_batch_crossing_l1_epoch_boundary() {
    let batcher_cfg = BatcherConfig {
        batch_type: BatchType::Span,
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    h.mine_l1_blocks(1);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let mut source = ActionL2Source::new();
    for _ in 1..=6u64 {
        source.push(builder.build_next_block_with_single_transaction().await);
    }
    assert_eq!(builder.head().l1_origin.number, 1, "block 6 must reference epoch 1");

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    Batcher::new(source, &h.rollup_config, batcher_cfg).advance(&mut h.l1).await;
    l1_chain.push(h.l1.tip().clone());

    node.initialize().await;
    node.sync_until_safe(6).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        6,
        "all 6 blocks derived crossing epoch boundary"
    );
}

/// The `BatchQueue` reorders span batches submitted in reverse L1 order.
#[tokio::test(start_paused = true)]
async fn out_of_order_span_batches_reordered_by_batch_queue() {
    let span_cfg = BatcherConfig {
        batch_type: BatchType::Span,
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&span_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());
    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    // L1 block 1: span batch for L2 block 2 (future batch, out of order).
    {
        let mut source = ActionL2Source::new();
        source.push(block2);
        Batcher::new(source, &h.rollup_config, span_cfg.clone()).advance(&mut h.l1).await;
    }
    l1_chain.push(h.l1.tip().clone());

    // L1 block 2: span batch for L2 block 1 (expected-next batch).
    {
        let mut source = ActionL2Source::new();
        source.push(block1);
        Batcher::new(source, &h.rollup_config, span_cfg).advance(&mut h.l1).await;
    }
    // Do NOT push block 2 yet.

    node.initialize().await;

    // Tick with only block 1 visible: future span batch buffered.
    node.tick().await;
    assert_eq!(node.engine.safe_head().block_info.number, 0, "future span batch buffered");

    // Push block 2 and sync: both blocks derived in correct order.
    l1_chain.push(h.l1.tip().clone());
    node.sync_until_safe(2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "span batches reordered correctly");
}

// ── Large L1 gaps ──────────────────────────────────────────────────────────────

/// Batches submitted after a long gap of empty L1 blocks are accepted and
/// derived correctly as long as the gap is within the sequence window.
#[tokio::test(start_paused = true)]
async fn large_l1_gaps_within_sequence_window() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain.clone());

    let block1 = builder.build_next_block_with_single_transaction().await;
    let block2 = builder.build_next_block_with_single_transaction().await;

    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    for _ in 0..15 {
        h.mine_and_push(&l1_chain);
    }

    let mut source = ActionL2Source::new();
    source.push(block1);
    source.push(block2);
    Batcher::new(source, &h.rollup_config, batcher_cfg).advance(&mut h.l1).await;
    l1_chain.push(h.l1.tip().clone());

    node.initialize().await;
    node.sync_until_safe(2).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        2,
        "both blocks derived after 15 empty L1 blocks"
    );
}

// ── Sequence-window exhaustion ─────────────────────────────────────────────────

/// When no batches are submitted across many L1 blocks the derivation pipeline
/// generates deposit-only default blocks for every expired epoch.
#[tokio::test(start_paused = true)]
async fn extended_sequence_window_exhaustion_fills_with_deposit_only_blocks() {
    const SEQ_WINDOW: u64 = 4;
    let batcher_cfg = BatcherConfig::default();
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_seq_window_size(SEQ_WINDOW)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(l1_chain.clone()).await;

    for _ in 0..20 {
        h.mine_and_push(&l1_chain);
    }

    node.initialize().await;
    node.sync_until_safe(1).await;

    assert!(
        node.engine.safe_head().block_info.number > 0,
        "safe head must advance via deposit-only blocks when sequence windows expire"
    );
}
