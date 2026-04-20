//! Derivation test across the Base Azul activation boundary.

use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TestRollupConfigBuilder,
};
use base_batcher_encoder::{DaType, EncoderConfig};

/// Derives 4 L2 blocks across the Base Azul activation boundary (ts=4, block 2)
/// and asserts each block includes 1 user transaction.
#[tokio::test(start_paused = true)]
async fn azul_derivation_crosses_activation_boundary() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..BatcherConfig::default()
    };

    // All Optimism forks through Jovian active from genesis; Base Azul at ts=4.
    // With block_time=2 and L2 genesis at ts=0:
    //   block 1 → ts=2  (pre-Base Azul)
    //   block 2 → ts=4  (first Base Azul block)
    //   block 3 → ts=6  (post-Base Azul)
    //   block 4 → ts=8  (post-Base Azul)
    let base_azul_time = 4u64;
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .through_isthmus()
        .with_jovian_at(0)
        .with_azul_at(base_azul_time)
        .build();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder = h.create_l2_sequencer(l1_chain);

    // Build and batch all 4 blocks.
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    for _ in 1..=4u64 {
        batcher.push_block(builder.build_next_block_with_single_transaction().await);
        batcher.advance(&mut h.l1).await;
    }

    let chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let node = h.create_actor_derivation_node(chain).await;
    node.initialize().await;
    node.sync_until_safe(4).await;

    assert_eq!(
        node.engine.safe_head().block_info.number,
        4,
        "safe head should advance past the Base Azul activation boundary"
    );

    // Verify each block includes exactly 1 user transaction (2 total: 1 L1 info deposit + 1 user).
    for i in 1u64..=4 {
        assert_eq!(
            node.engine.executed_tx_count(i),
            2,
            "L2 block {i} should contain 1 user transaction (total 2: deposit + user)"
        );
    }
}
