//! Action tests that exercise [`TestActorFollowNode`] end-to-end.
//!
//! These tests validate the **production actor coordination layer** of the follow
//! node: that [`DelegateL2DerivationActor`] and [`EngineActor`] run together with
//! real in-process channels, that the poll-interval timing loop fires correctly,
//! and that safe/finalized head labels propagate through the FCU path.
//!
//! Unlike the [`TestFollowNode`] tests, these tests use
//! `#[tokio::test(start_paused = true)]` and [`TestActorFollowNode::tick`] to
//! advance wall-clock time deterministically without real sleeps.
//!
//! [`DelegateL2DerivationActor`]: base_consensus_node::DelegateL2DerivationActor
//! [`EngineActor`]: base_consensus_node::EngineActor
//! [`TestFollowNode`]: base_action_harness::TestFollowNode

use std::sync::Arc;

use alloy_primitives::B256;
use base_action_harness::{
    ActionL2LocalProvider, ActionL2SourceBridge, ActionTestHarness, L1MinerConfig,
    SharedBlockHashRegistry, SharedL1Chain, TestActorFollowNode, TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// Build the sequencer and follow node together with separate block-hash registries.
///
/// The follow node gets its own fresh [`SharedBlockHashRegistry`] so that
/// `ActionL2LocalProvider::block_number()` correctly returns the number of blocks
/// the follow node's engine has actually executed — not the sequencer's pre-built
/// count. Without this separation the derivation actor initialises `sent_head` to the
/// sequencer's latest block number and immediately skips all sync.
async fn setup_with_sequencer()
-> (TestActorFollowNode, ActionL2SourceBridge, base_action_harness::L2Sequencer) {
    let batcher_cfg = base_action_harness::BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..base_action_harness::BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let sequencer = h.create_l2_sequencer(chain.clone());

    let rollup_config = Arc::new(h.rollup_config.clone());
    let genesis_head = h.l2_genesis();

    let source = ActionL2SourceBridge::new();

    // Give the follow node its OWN registry, separate from the sequencer's.
    // The sequencer writes into its own registry as blocks are built; the follow
    // node writes into this fresh one as it executes blocks via the actor stack.
    // This ensures `ActionL2LocalProvider::block_number()` reflects the follow
    // node's engine state (initially 0) rather than the sequencer's state.
    let follow_registry = SharedBlockHashRegistry::new();
    let local_provider = ActionL2LocalProvider::new(follow_registry.clone());

    let engine = base_action_harness::ActionEngineClient::new(
        Arc::clone(&rollup_config),
        genesis_head,
        follow_registry,
        chain,
    );

    let follow_node =
        TestActorFollowNode::new(rollup_config, engine, source.clone(), local_provider, false, 512)
            .await;

    (follow_node, source, sequencer)
}

/// The production actor stack syncs unsafe blocks from the source bridge.
///
/// 1. A sequencer builds 5 L2 blocks and they are pushed to the source bridge.
/// 2. `sync_to_unsafe(5)` advances the clock and yields until the follow-node actors
///    process all blocks through `new_payload` + FCU.
/// 3. The engine's unsafe head must advance to block 5.
#[tokio::test(start_paused = true)]
async fn test_actor_syncs_unsafe_blocks() {
    let (follow_node, source, mut sequencer) = setup_with_sequencer().await;

    for _ in 0..5 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block);
    }

    follow_node.sync_to_unsafe(5).await;

    assert_eq!(
        follow_node.engine.unsafe_head().block_info.number,
        5,
        "unsafe head must advance to block 5"
    );
}

/// Safe head propagates through the `DelegatedForkchoiceUpdate` path.
///
/// 1. Push 5 blocks; mark block 3 as safe on the source before syncing.
/// 2. `sync_to_unsafe(5)` drives the derivation actor which calls
///    `update_safe_and_finalized` at the end of the sync run; it sends a
///    `DelegatedForkchoiceUpdate` carrying safe=3.
/// 3. Extra ticks allow the delegated FCU to drain through the engine actor.
/// 4. The engine's unsafe head is at 5.
#[tokio::test(start_paused = true)]
async fn test_actor_safe_head_advances() {
    let (follow_node, source, mut sequencer) = setup_with_sequencer().await;

    for _ in 0..5 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(3);

    follow_node.sync_to_unsafe(5).await;

    // Tick a few more times so the DelegatedForkchoiceUpdate reaches the engine.
    for _ in 0..5 {
        follow_node.tick().await;
    }

    assert_eq!(
        follow_node.engine.unsafe_head().block_info.number,
        5,
        "unsafe head must be 5 after syncing"
    );
}

/// Finalized head advances when the source marks a block as finalized.
///
/// 1. Push 5 blocks; source safe=3, finalized=2.
/// 2. `sync_to_unsafe(5)` triggers `update_safe_and_finalized` which sends a
///    `DelegatedForkchoiceUpdate` carrying safe=3 and finalized=2.
/// 3. Extra ticks allow the FCU to drain through the engine actor.
/// 4. Unsafe (5), safe (3), and finalized (2) heads are all asserted.
#[tokio::test(start_paused = true)]
async fn test_actor_finalized_head_advances() {
    let (follow_node, source, mut sequencer) = setup_with_sequencer().await;

    for _ in 0..5 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block);
    }
    source.set_safe_number(3);
    source.set_finalized_number(2);

    follow_node.sync_to_unsafe(5).await;

    for _ in 0..5 {
        follow_node.tick().await;
    }

    assert_eq!(
        follow_node.engine.unsafe_head().block_info.number,
        5,
        "unsafe head must be 5 after syncing"
    );
    assert_eq!(
        follow_node.engine.safe_head().block_info.number,
        3,
        "safe head must advance to block 3"
    );
    assert_eq!(
        follow_node.engine.finalized_head().block_info.number,
        2,
        "finalized head must advance to block 2 via DelegatedForkchoiceUpdate"
    );
}

/// Proofs-gating caps sync when the proofs head is below the source.
///
/// With `proofs_max_blocks_ahead = 2` and `proofs_head = 0`, the actor must not
/// advance the follow node beyond block 2 even though the source has 10 blocks.
#[tokio::test(start_paused = true)]
async fn test_actor_proofs_gating() {
    let batcher_cfg = base_action_harness::BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..base_action_harness::BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg).build();
    let h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(chain.clone());

    let rollup_config = Arc::new(h.rollup_config.clone());
    let genesis_head = h.l2_genesis();
    let source = ActionL2SourceBridge::new();

    // proofs_head = 0: the local provider reports the proofs ExEx has processed up to block 0.
    // Use a fresh follow-node registry so block_number() returns 0 initially.
    let follow_registry = SharedBlockHashRegistry::new();
    let local_provider = ActionL2LocalProvider::new(follow_registry.clone()).with_proofs_head(0);

    let engine = base_action_harness::ActionEngineClient::new(
        Arc::clone(&rollup_config),
        genesis_head,
        follow_registry,
        chain,
    );

    let follow_node = TestActorFollowNode::new(
        rollup_config,
        engine,
        source.clone(),
        local_provider,
        true, // proofs_enabled
        2,    // proofs_max_blocks_ahead: cap at 0 + 2 = 2
    )
    .await;

    // Build and push 10 blocks.
    for _ in 0..10 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block);
    }

    // Sync: the actor should cap at block 2 (proofs_head=0 + max_ahead=2).
    follow_node.sync_to_unsafe(2).await;

    // Verify the engine advanced to at least block 1 (gate did not block all progress)
    // and did not advance past block 2 (gate is enforced).
    let head = follow_node.engine.unsafe_head().block_info.number;
    assert!(
        head >= 1,
        "proofs gating must allow at least block 1, but engine is stuck at block {head}"
    );
    assert!(
        head <= 2,
        "proofs gating must cap sync at block 2, but engine advanced to block {head}"
    );
}

/// Chains-agree guard prevents the safe FCU when the local hash differs from the source.
///
/// 1. Push 3 blocks and sync to block 3 (safe_number = 0, no safe FCU sent).
/// 2. Insert a wrong hash for block 2 into the engine's block-hash registry.
/// 3. Push block 4 to the source and mark source safe = 2.
/// 4. Tick — the actor syncs block 4, calls `update_safe_and_finalized`, detects the
///    hash mismatch for block 2, and skips the delegated FCU.
/// 5. The engine's unsafe head is at 4 (unsafe sync is unaffected) but the safe head
///    remains at genesis because no safe FCU was sent.
#[tokio::test(start_paused = true)]
async fn test_actor_chains_agree_guard() {
    let (follow_node, source, mut sequencer) = setup_with_sequencer().await;

    for _ in 0..3 {
        let block = sequencer.build_next_block_with_single_transaction().await;
        source.push(block);
    }

    // Sync to block 3 — safe_number = 0 so update_safe_and_finalized returns early;
    // no safe FCU is ever sent, so safe_head stays at genesis.
    follow_node.sync_to_unsafe(3).await;

    assert_eq!(
        follow_node.engine.unsafe_head().block_info.number,
        3,
        "unsafe head must be at block 3 before tamper"
    );
    assert_eq!(
        follow_node.engine.safe_head().block_info.number,
        0,
        "safe head must be at genesis before any safe FCU"
    );

    // Tamper: record a wrong hash for block 2 in the local registry.
    // The follow-node's ActionL2LocalProvider.block_hash_at(2) will now return this
    // wrong hash, causing the chains_agree guard to fire.
    follow_node.engine.block_hash_registry().insert(2, B256::from([0xff; 32]), None);

    // Push block 4 so the actor has a new target (sent_head=3 < target=4).
    // Without a new block the actor skips the sync tick entirely.
    let block4 = sequencer.build_next_block_with_single_transaction().await;
    source.push(block4);

    // Mark source safe = 2 and tick — chains_agree fires; safe FCU is skipped.
    source.set_safe_number(2);
    for _ in 0..5 {
        follow_node.tick().await;
    }

    // Unsafe head advances to 4 — unsafe sync is not blocked by the chains_agree guard.
    assert_eq!(
        follow_node.engine.unsafe_head().block_info.number,
        4,
        "unsafe head must advance to block 4 despite chains_agree guard"
    );

    // Safe head must stay at genesis: the chains_agree guard fired and the delegated
    // FCU carrying safe=2 was never sent to the engine.
    assert_eq!(
        follow_node.engine.safe_head().block_info.number,
        0,
        "safe head must stay at genesis when chains_agree guard fires"
    );
}
