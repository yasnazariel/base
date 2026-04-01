//! End-to-end integration test for the virtual address registry.
//!
//! Demonstrates the full flow:
//! 1. Register a virtual master via a transaction to the registry address
//! 2. Verify the registration in storage
//! 3. Construct a virtual address and verify resolution
//! 4. Build blocks containing registry interactions
//! 5. Batch all blocks and submit to L1

use alloy_primitives::{Bytes, TxKind, U256, address};
use alloy_sol_types::SolCall;
use base_action_harness::{
    ActionL2Source, ActionTestHarness, Batcher, BatcherConfig, L1MinerConfig, SharedL1Chain,
    TEST_ACCOUNT_ADDRESS, TestRollupConfigBuilder,
};
use base_address_resolution::{MasterId, REGISTRY_ADDRESS, UserTag, VirtualAddress};
use base_batcher_encoder::{DaType, EncoderConfig};
use virtual_address_registry::{IAddressRegistry, RegistryEvmOverride, StorageBackedRegistry};

/// Virtual address registry: register, resolve, and batch.
#[tokio::test]
async fn virtual_address_registry_e2e() {
    let batcher_cfg = BatcherConfig {
        encoder: EncoderConfig { da_type: DaType::Calldata, ..EncoderConfig::default() },
        ..Default::default()
    };

    // Use through_isthmus + jovian_at(0) so we're on a recent hardfork.
    let rollup_cfg = TestRollupConfigBuilder::base_mainnet(&batcher_cfg)
        .through_isthmus()
        .with_jovian_at(0)
        .build();
    let chain_id = rollup_cfg.l2_chain_id.id();
    let mut h = ActionTestHarness::new(L1MinerConfig::default(), rollup_cfg);

    let l1_chain = SharedL1Chain::from_blocks(h.l1.chain().to_vec());
    let mut builder =
        h.create_l2_sequencer_with_evm_override(l1_chain, Box::new(RegistryEvmOverride));

    let account = builder.test_account();

    // ── Step 1: Pre-seed registry state ────────────────────────────────
    //
    // Write a registration directly into the sequencer's EVM database.
    // In production, this would happen via a registerVirtualMaster transaction
    // that passes the 32-bit PoW. For testing, we bypass PoW and write directly.
    let master_address = TEST_ACCOUNT_ADDRESS;
    let master_id = MasterId::from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    StorageBackedRegistry::write_master(builder.db_mut(), master_id, master_address);

    // ── Step 2: Verify registration ────────────────────────────────────
    {
        let resolved = StorageBackedRegistry::read_master(builder.db(), master_id);
        assert_eq!(resolved, Some(master_address), "master must be registered after write");
    }

    // ── Step 3: Verify virtual address resolution ──────────────────────
    let user_tag = UserTag::from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    let virtual_addr = VirtualAddress::encode(master_id, user_tag);

    assert!(
        VirtualAddress::is_virtual(virtual_addr),
        "encoded address must be recognized as virtual"
    );

    {
        let resolved = StorageBackedRegistry::resolve(builder.db(), virtual_addr);
        assert_eq!(
            resolved,
            Ok(master_address),
            "virtual address must resolve to registered master"
        );
    }

    // Non-virtual address passes through unchanged.
    {
        let regular_addr = address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        let resolved = StorageBackedRegistry::resolve(builder.db(), regular_addr);
        assert_eq!(resolved, Ok(regular_addr), "non-virtual address must pass through unchanged");
    }

    // ── Step 4: Build blocks with transactions ─────────────────────────
    //
    // Block 1: empty (advances the chain past genesis).
    let block1 = builder.build_empty_block();

    // Block 2: user transfer (exercises default EVM execution via override).
    let tx = {
        let mut acct = account.lock().expect("test account lock");
        acct.create_tx(
            chain_id,
            TxKind::Call(alloy_primitives::Address::ZERO),
            Bytes::new(),
            U256::from(1),
            21_000,
        )
    };
    let block2 = builder.build_next_block_with_transactions(vec![tx]);

    // Block 3: send a registerVirtualMaster call to the registry address.
    // This exercises the EvmOverride's interception of registry calls.
    let register_calldata =
        IAddressRegistry::registerVirtualMasterCall { salt: alloy_primitives::FixedBytes::ZERO }
            .abi_encode();

    let registry_tx = {
        let mut acct = account.lock().expect("test account lock");
        acct.create_tx(
            chain_id,
            TxKind::Call(REGISTRY_ADDRESS),
            Bytes::from(register_calldata),
            U256::ZERO,
            100_000,
        )
    };
    let block3 = builder.build_next_block_with_transactions(vec![registry_tx]);

    // ── Step 5: Verify post-execution state ────────────────────────────
    //
    // The registry write from step 1 should still be present.
    {
        let resolved = StorageBackedRegistry::read_master(builder.db(), master_id);
        assert_eq!(resolved, Some(master_address), "registry mapping must survive block execution");
    }

    // Virtual address resolution still works after blocks.
    {
        let resolved = StorageBackedRegistry::resolve(builder.db(), virtual_addr);
        assert_eq!(
            resolved,
            Ok(master_address),
            "virtual address must still resolve after block execution"
        );
    }

    // ── Step 6: Batch blocks and submit to L1 ──────────────────────────
    let mut batcher = Batcher::new(ActionL2Source::new(), &h.rollup_config, batcher_cfg.clone());
    let _l1_chain_for_batcher = SharedL1Chain::from_blocks(h.l1.chain().to_vec());

    for block in [block1, block2, block3] {
        batcher.push_block(block);
        batcher.advance(&mut h.l1).await;
    }

    // Verify that L1 blocks were mined containing batch data.
    assert!(h.l1.latest_number() >= 3, "L1 should have mined blocks for the batch submissions");

    // Verify the batcher produced valid L1 transactions.
    let mut found_batch_tx = false;
    for i in 1..=h.l1.latest_number() {
        let block = h.l1.block_by_number(i).expect("L1 block must exist");
        if !block.batcher_txs.is_empty() {
            found_batch_tx = true;
        }
    }
    assert!(found_batch_tx, "at least one L1 block must contain a batcher transaction");
}

/// Virtual address format: encode/decode roundtrip with resolution.
#[test]
fn virtual_address_roundtrip() {
    let master_id = MasterId::from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
    let user_tag = UserTag::from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);

    let addr = VirtualAddress::encode(master_id, user_tag);
    assert!(VirtualAddress::is_virtual(addr));

    let (decoded_mid, decoded_ut) = VirtualAddress::decode(addr).unwrap();
    assert_eq!(decoded_mid, master_id);
    assert_eq!(decoded_ut, user_tag);
}

/// Unregistered virtual address resolution returns an error.
#[test]
fn unregistered_virtual_address_reverts() {
    use revm::database::InMemoryDB;

    let master_id = MasterId::from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let user_tag = UserTag::from_slice(&[0x00; 6]);
    let addr = VirtualAddress::encode(master_id, user_tag);

    let db = InMemoryDB::default();
    let result = StorageBackedRegistry::resolve(&db, addr);
    assert_eq!(
        result,
        Err(base_address_resolution::RegistryError::VirtualAddressUnregistered),
        "unregistered virtual address must return error"
    );
}
