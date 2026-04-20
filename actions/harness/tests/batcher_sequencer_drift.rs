//! TDD action test skeletons for sequencer drift scenarios.

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

// ---------------------------------------------------------------------------
// A. Sequencer drift — L2 timestamp exceeds L1 origin time + max_sequencer_drift
// ---------------------------------------------------------------------------

/// When the L2 sequencer is pinned to a stale L1 origin and builds enough
/// blocks that `L2_timestamp > L1_origin_time + max_sequencer_drift`, the
/// derivation pipeline should still derive those blocks — but any non-empty
/// batch (one containing user transactions) whose timestamp is past the drift
/// boundary is dropped. Only deposit-only (default) blocks are produced for
/// the over-drift slots.
///
/// ## Setup
///
/// - Fjord active → `max_sequencer_drift = 1800 s`, `block_time = 300 s`, L1
///   `block_time = 4 s`
/// - L1 genesis at ts=0 → L1 block 1 at ts=4
/// - Pin the sequencer to L1 genesis (epoch 0, ts=0)
/// - Build L2 blocks: ts=300, 600, …, 1800, 2100, 2400
/// - After L2 block 6 (ts=1800), `1800 ≤ 0 + 1800 = 1800` → still within
/// - L2 block 7 (ts=2100): `2100 > 1800` → drift exceeded
///
/// ## Expected behaviour
///
/// The derivation pipeline:
/// 1. Accepts L2 blocks 1-6 (timestamps 300-1800, within drift) as submitted
/// 2. For L2 blocks 7-8 (timestamps 2100-2400, over drift), drops the
///    batcher's non-empty batch and generates deposit-only default blocks
#[tokio::test(start_paused = true)]
async fn sequencer_drift_produces_deposit_only_blocks() {
    let l1_cfg = L1MinerConfig { block_time: 4 };
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .with_block_time(300)
        // Small sequence window so the pipeline generates deposit-only blocks for the
        // over-drift slots once the window expires. With seq_window_size=2 and the batch
        // submitted in L1 block 2, the window expires at L1 block 3 (epoch 0 + 2 < 3),
        // prompting the pipeline to auto-generate default blocks for slots 7 and 8.
        .with_seq_window_size(2)
        .build();
    let mut h = ActionTestHarness::new(l1_cfg, rollup_cfg.clone());

    // Mine L1 block 1 (ts=4) so the sequencer has an epoch to reference,
    // but we will PIN the sequencer to epoch 0 (ts=0) to force drift.
    h.mine_l1_blocks(1);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);

    // Pin the sequencer to L1 genesis (epoch 0, ts=0).
    let l1_genesis = h.l1.block_info_at(0);
    sequencer.pin_l1_origin(l1_genesis);

    // Build 8 L2 blocks pinned to epoch 0 (block_time=300 s, max_drift=1800 s):
    //   block 1: ts= 300 (drift=  300 ≤ 1800) ✓
    //   block 2: ts= 600 (drift=  600 ≤ 1800) ✓
    //   block 3: ts= 900 (drift=  900 ≤ 1800) ✓
    //   block 4: ts=1200 (drift= 1200 ≤ 1800) ✓
    //   block 5: ts=1500 (drift= 1500 ≤ 1800) ✓
    //   block 6: ts=1800 (drift= 1800 ≤ 1800) ✓ (exactly at boundary)
    //   block 7: ts=2100 (drift= 2100 > 1800) ✗ over drift
    //   block 8: ts=2400 (drift= 2400 > 1800) ✗ over drift
    //
    // Blocks 1-6 have user transactions. Blocks 7-8 also have user txs
    // (sequencer doesn't enforce drift), but the pipeline should drop them.

    // Collect all 8 blocks and batch them in one L1 block.
    let mut source = ActionL2Source::new();
    for _ in 1u64..=8 {
        source.push(sequencer.build_next_block_with_single_transaction().await);
    }

    let mut batcher = Batcher::new(source, &h.rollup_config, batcher_cfg.clone());
    batcher.advance(&mut h.l1).await; // L1 block 2: batch for all 8 L2 blocks

    // Mine 2 extra empty L1 blocks (seq_window_size=2, batch epoch=0, batch
    // in L1 block 2 → window expires at L1 block 3). The pipeline needs to
    // see L1 blocks 3 and 4 to auto-generate deposit-only blocks for slots 7-8:
    // block 3 produces slot 7, block 4 produces slot 8.
    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    h.mine_and_push(&chain); // L1 block 3
    h.mine_and_push(&chain); // L1 block 4

    // The actor derivation node starts with an empty block-hash registry,
    // so state-root validation is skipped for all blocks including 7-8
    // (which differ from the sequencer's version: deposit-only vs. user-tx).
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;
    node.sync_until_safe(8).await;

    // The pipeline should derive blocks for all L2 slots. Blocks 1-6 use the
    // batcher's submitted batches. Blocks 7-8 are generated as deposit-only
    // default blocks because the non-empty batches are dropped for exceeding
    // max_sequencer_drift.
    assert_eq!(
        node.engine.safe_head().block_info.number,
        8,
        "all 8 L2 blocks must be derived (blocks 7-8 as deposit-only over-drift blocks)"
    );

    // Verify deposit-only behaviour: blocks 1-6 carry 2 txs each (deposit +
    // user tx), blocks 7-8 must carry exactly 1 tx (L1 info deposit only).
    for number in 1u64..=8 {
        let count = node.engine.executed_tx_count(number);
        if number <= 6 {
            assert_eq!(count, 2, "block {number} should have deposit + user tx");
        } else {
            assert_eq!(count, 1, "block {number} past drift boundary should be deposit-only");
        }
    }
}

// ---------------------------------------------------------------------------
// B. Sequencer drift with forced-empty blocks
// ---------------------------------------------------------------------------

/// When `max_sequencer_drift` is exceeded, the sequencer should produce
/// deposit-only (empty) blocks. This test verifies that the pipeline correctly
/// handles the over-drift region by deriving blocks for all L2 slots that are
/// within the drift boundary, even when the submitted batches for over-drift
/// slots are dropped.
///
/// This test uses `L2Sequencer::build_empty_block()` for the over-drift
/// blocks (7-8). The pipeline drops those batches (they still reference the
/// stale epoch 0, triggering `SequencerDriftNotAdoptedNextOrigin`). With the
/// default `seq_window_size`, the window does not expire from the small number
/// of L1 blocks mined here, so blocks 7-8 are never auto-generated. The safe
/// head stops at 6.
#[tokio::test(start_paused = true)]
async fn sequencer_drift_forced_empty_blocks_accepted() {
    let l1_cfg = L1MinerConfig { block_time: 4 };
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };
    let rollup_cfg =
        TestRollupConfigBuilder::base_mainnet(&batcher_cfg).with_block_time(300).build();
    let mut h = ActionTestHarness::new(l1_cfg, rollup_cfg);

    // Mine 1 L1 block so epoch 1 exists, but pin sequencer to epoch 0.
    h.mine_l1_blocks(1);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut sequencer = h.create_l2_sequencer(l1_chain);
    let l1_genesis = h.l1.block_info_at(0);
    sequencer.pin_l1_origin(l1_genesis);

    // Build 6 normal blocks (within drift, ts=300..1800) + 2 empty blocks
    // (over drift, ts=2100, 2400). block_time=300 s, max_drift=1800 s.
    let mut source = ActionL2Source::new();
    for _ in 1u64..=6 {
        source.push(sequencer.build_next_block_with_single_transaction().await);
    }
    // Build empty blocks past the drift boundary. The empty block has only
    // the deposit tx — the batcher encodes it but the pipeline drops it
    // (stale epoch) and the default seq_window_size is too large to expire,
    // so no default block is generated for these slots.
    for _ in 7u64..=8 {
        source.push(sequencer.build_empty_block().await);
    }

    let mut batcher = Batcher::new(source, &h.rollup_config, batcher_cfg.clone());
    batcher.advance(&mut h.l1).await; // L1 block 2: all 8 batches

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;

    // Derive blocks 1-6 (within drift). Blocks 7-8 are dropped and not
    // replaced (seq_window doesn't expire) — safe head stops at 6.
    node.sync_until_safe(6).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        6,
        "blocks 1-6 must derive; over-drift blocks 7-8 are dropped and seq window does not expire"
    );
}
