//! Action tests that exercise [`TestActorDerivationNode`] end-to-end.
//!
//! These tests validate the **production actor coordination layer** of the
//! rollup node: that [`DerivationActor`] and [`EngineActor`] run together with
//! real in-process channels, that the full 8-stage derivation pipeline
//! processes real batcher transactions, and that safe / finalized head labels
//! propagate through the correct code paths.
//!
//! Unlike the [`TestRollupNode`] tests which step the pipeline manually, these
//! tests use the real actor inbox: L1 head updates are delivered as
//! [`DerivationActorRequest::ProcessL1HeadUpdateRequest`] messages, engine
//! confirmations flow back as
//! [`DerivationActorRequest::ProcessEngineSafeHeadUpdateRequest`] messages, and
//! state-machine gating (one block at a time, `AwaitingSafeHeadConfirmation` →
//! `Deriving`) is exercised on every derived block.
//!
//! [`DerivationActor`]: base_consensus_node::DerivationActor
//! [`EngineActor`]: base_consensus_node::EngineActor
//! [`TestRollupNode`]: base_action_harness::TestRollupNode

use std::sync::Arc;

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// Helper to build a harness and pre-submit `n` L2 blocks, one batch per L1
/// block, returning `(harness, l1_chain, l1_tip_info)`.
async fn setup_with_n_blocks(
    n: u64,
) -> (ActionTestHarness, SharedL1Chain) {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg);
    for _ in 0..n {
        let block = sequencer.build_next_block_with_single_transaction().await;
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    (h, chain)
}

/// The production actor stack derives L2 blocks from L1 batch data.
///
/// 1. Build 3 L2 blocks, submit one batch per L1 block.
/// 2. Create `TestActorDerivationNode`, initialize it.
/// 3. Signal the L1 tip; let the actor stack derive all 3 blocks.
/// 4. Assert the engine's safe head advanced to block 3.
#[tokio::test]
async fn test_actor_derives_basic_l2_blocks() {
    let (h, chain) = setup_with_n_blocks(3).await;

    let l1_tip = base_action_harness::block_info_from(h.l1.tip());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;
    node.sync_until_safe(3, l1_tip).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        3,
        "safe head must advance to block 3 after derivation"
    );
}

/// The engine's unsafe head mirrors the safe head in derivation mode.
///
/// Each derived block goes through `InsertTask` (sets canonical head) and
/// `ConsolidateTask` (FCU with safe=head). Both `unsafe_head` and `safe_head`
/// on the [`ActionEngineClient`] should agree after sync.
#[tokio::test]
async fn test_actor_unsafe_equals_safe_after_derivation() {
    let (h, chain) = setup_with_n_blocks(5).await;

    let l1_tip = base_action_harness::block_info_from(h.l1.tip());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(5, l1_tip).await;

    let safe = node.engine.safe_head().block_info.number;
    let unsafe_ = node.engine.unsafe_head().block_info.number;
    assert_eq!(safe, 5, "safe head must be 5");
    assert_eq!(unsafe_, safe, "unsafe head must equal safe head in derivation mode");
}

/// The [`SafeDB`] is written for each derived block.
///
/// Each L2 block's safe head is recorded by the derivation actor's
/// `ProcessEngineSafeHeadUpdateRequest` handler. Querying at `l1_block_num =
/// u64::MAX` must return the most recently derived safe head.
#[tokio::test]
async fn test_actor_safe_head_db_written() {
    let (h, chain) = setup_with_n_blocks(3).await;

    let l1_tip = base_action_harness::block_info_from(h.l1.tip());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(3, l1_tip).await;

    // SafeDB must have been written for L2 block 3. Query at u64::MAX to
    // get the latest entry regardless of the exact L1 inclusion block.
    let response = node
        .safe_head_at_l1(u64::MAX)
        .await
        .expect("SafeDB must have an entry after derivation");
    assert_eq!(
        response.safe_head.number, 3,
        "SafeDB must record L2 block 3 as the safe head"
    );
}

/// Finalization propagates to the engine when the L1 finalizes a block whose
/// epoch contains derived L2 blocks.
///
/// 1. Derive 3 L2 blocks.
/// 2. Send `act_l1_finalized_signal` for L1 block 1 (the epoch of L2 block 1).
/// 3. Tick a few more times so the `L2Finalizer` can promote the L2 finalized.
/// 4. The engine's finalized head must advance beyond genesis.
///
/// Note: the `L2Finalizer` only promotes the L2 finalized head once a
/// `ProcessFinalizedL2BlockNumberRequest` is sent to the engine actor, which
/// happens when `try_finalize_next` in `DerivationActor` finds a matching
/// entry in its finalization queue.
#[tokio::test]
async fn test_actor_finalization_advances() {
    let (h, chain) = setup_with_n_blocks(3).await;

    let l1_tip = base_action_harness::block_info_from(h.l1.tip());
    let l1_block_1 = h.l1.block_by_number(1).map(base_action_harness::block_info_from)
        .expect("L1 block 1 must exist");
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(3, l1_tip).await;

    // Finalize L1 block 1. The `L2Finalizer` will check its pending queue for
    // L2 blocks whose L1 inclusion block is ≤ L1 block 1. L2 block 1 was
    // included in L1 block 1, so it should be eligible for finalization.
    node.act_l1_finalized_signal(l1_block_1).await;
    node.tick().await;
    node.tick().await;

    // The engine's finalized head must have advanced beyond genesis (block 0).
    let fin = node.engine.finalized_head().block_info.number;
    assert!(
        fin >= 1,
        "finalized head must advance to at least block 1 after L1 finalization, got {fin}"
    );
}

/// Multiple L2 blocks encoded in a single batcher channel are all derived from
/// one L1 block.
///
/// Submit 5 L2 blocks in a single `Batcher::advance` call (one L1 block) and
/// verify the actor stack derives all 5 from that single L1 inclusion block.
#[tokio::test]
async fn test_actor_multi_block_single_l1_inclusion() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    let mut source = ActionL2Source::new();
    for _ in 0..5 {
        source.push(sequencer.build_next_block_with_single_transaction().await);
    }
    // Encode all 5 blocks into a single batcher channel / L1 block.
    Batcher::new(source, &h.rollup_config, batcher_cfg).advance(&mut h.l1).await;

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let l1_tip = base_action_harness::block_info_from(h.l1.tip());

    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    node.sync_until_safe(5, l1_tip).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        5,
        "all 5 L2 blocks must be derived from a single L1 inclusion block"
    );
}

/// Sending an L1 head signal when no batcher data is available leaves the safe
/// head at genesis.
///
/// The derivation actor transitions to `Deriving`, the pipeline immediately
/// hits EOF (`NotEnoughData` / `Eof`), and the state machine goes to
/// `AwaitingL1Data`. No L2 block is derived.
#[tokio::test]
async fn test_actor_no_data_eof_stays_at_genesis() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    // Chain with only genesis — no batches submitted.
    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let l1_tip = base_action_harness::block_info_from(h.l1.tip());

    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    // Signal L1 head — pipeline should hit EOF immediately.
    node.act_l1_head_signal(l1_tip).await;
    node.tick().await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        0,
        "safe head must stay at genesis when no L1 batch data is available"
    );
}

/// After derivation, additional L1 blocks with more batches extend the safe
/// head further.
///
/// Simulate the incremental batch submission pattern: first 2 blocks, then
/// 3 more. Each round of L1 data arrives with a new L1 head signal.
#[tokio::test]
async fn test_actor_incremental_derivation() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain_for_seq = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain_for_seq);

    // Round 1: submit 2 L2 blocks.
    let mut batcher1 = Batcher::new(
        ActionL2Source::new(),
        &h.rollup_config,
        batcher_cfg.clone(),
    );
    for _ in 0..2 {
        batcher1.push_block(sequencer.build_next_block_with_single_transaction().await);
        batcher1.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let l1_tip1 = Arc::new(base_action_harness::block_info_from(h.l1.tip()));

    let node = h.create_actor_derivation_node(chain.clone()).await;
    node.initialize().await;

    // First sync: derive 2 blocks.
    node.sync_until_safe(2, *l1_tip1).await;
    assert_eq!(node.engine.safe_head().block_info.number, 2, "first batch: safe head = 2");

    // Round 2: submit 3 more L2 blocks and push to the shared chain.
    let mut batcher2 = Batcher::new(
        ActionL2Source::new(),
        &h.rollup_config,
        batcher_cfg.clone(),
    );
    for _ in 0..3 {
        batcher2.push_block(sequencer.build_next_block_with_single_transaction().await);
        batcher2.advance(&mut h.l1).await;
        chain.push(h.l1.tip().clone());
    }
    let l1_tip2 = base_action_harness::block_info_from(h.l1.tip());

    // Second sync: derive blocks 3-5.
    node.sync_until_safe(5, l1_tip2).await;
    assert_eq!(node.engine.safe_head().block_info.number, 5, "second batch: safe head = 5");
}
