use alloy_evm::Database;
use alloy_primitives::{Address, Bytes};
use base_alloy_consensus::{NONCE_MANAGER_ADDRESS, TX_CONTEXT_ADDRESS};
use base_alloy_chains::BaseUpgrades;
use revm::{DatabaseCommit, primitives::HashMap, state::Bytecode};

/// Precompile addresses that need stub bytecode at activation.
///
/// Only native precompiles (NonceManager, TxContext) are included. Deployed
/// contracts (AccountConfiguration, verifiers, DefaultAccount) receive their
/// real bytecode via `TxDeposit` upgrade transactions at hardfork activation
/// (see `crates/consensus/upgrades/src/base_v1.rs`). On devnets they are
/// deployed by `deploy-8130.sh` since BASE_V1 is active from genesis.
const AA_PRECOMPILE_ADDRESSES: [Address; 2] = [NONCE_MANAGER_ADDRESS, TX_CONTEXT_ADDRESS];

/// Stub bytecode deployed to precompile addresses.
///
/// `0xFE` is the `INVALID` opcode -- any direct call reverts immediately.
/// The real logic is handled by the node as native precompiles in the EVM
/// handler. This stub ensures the accounts are non-empty under EIP-161,
/// preventing state cleanup from deleting their storage.
const AA_STUB_BYTECODE: &[u8] = &[0xFE];

/// The Base V1 hardfork issues an irregular state transition that force-deploys
/// stub bytecode to the EIP-8130 precompile addresses.
///
/// This mirrors `ensure_create2_deployer` for Canyon: code is set directly
/// via `DatabaseCommit` on the first block where the fork is active.
pub fn ensure_aa_predeploys<DB>(
    chain_spec: impl BaseUpgrades,
    timestamp: u64,
    db: &mut DB,
) -> Result<(), DB::Error>
where
    DB: Database + DatabaseCommit,
{
    if !chain_spec.is_base_v1_active_at_timestamp(timestamp) {
        return Ok(());
    }

    // Only deploy on the first BASE_V1 block, or if the sentinel
    // (NonceManager) still has no code. The second check handles
    // genesis-activated devnets where the first-block heuristic
    // (`timestamp - 2`) can't distinguish block 0 from block 1.
    let sentinel = db.basic(NONCE_MANAGER_ADDRESS)?;
    let already_deployed =
        sentinel.as_ref().is_some_and(|info| info.code_hash != revm::primitives::KECCAK_EMPTY);

    if already_deployed {
        return Ok(());
    }

    let code = Bytecode::new_raw(Bytes::from_static(AA_STUB_BYTECODE));
    let code_hash = code.hash_slow();

    let mut accounts = HashMap::default();
    for addr in AA_PRECOMPILE_ADDRESSES {
        let mut acc_info = db.basic(addr)?.unwrap_or_default();
        acc_info.code_hash = code_hash;
        acc_info.code = Some(code.clone());

        let mut revm_acc: revm::state::Account = acc_info.into();
        revm_acc.mark_touch();
        accounts.insert(addr, revm_acc);
    }

    db.commit(accounts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use revm::{Database, database::InMemoryDB};

    use super::*;

    fn devnet_spec() -> base_alloy_upgrades::BaseChainUpgrades {
        base_alloy_upgrades::BaseChainUpgrades::devnet()
    }

    fn make_db() -> revm::database::State<InMemoryDB> {
        revm::database::State::builder().with_database(InMemoryDB::default()).build()
    }

    #[test]
    fn deploys_precompile_stubs_on_activation() {
        let mut db = make_db();
        let spec = devnet_spec();

        ensure_aa_predeploys(&spec, 0, &mut db).unwrap();

        for addr in AA_PRECOMPILE_ADDRESSES {
            let info = db.basic(addr).unwrap().expect("account should exist");
            assert!(info.code.is_some(), "code missing for {addr}");
            assert_eq!(&info.code.unwrap().original_bytes()[..], &[0xFE]);
        }
    }

    #[test]
    fn idempotent_when_already_deployed() {
        let mut db = make_db();
        let spec = devnet_spec();

        ensure_aa_predeploys(&spec, 0, &mut db).unwrap();

        let mut info = db.basic(NONCE_MANAGER_ADDRESS).unwrap().unwrap_or_default();
        info.balance = U256::from(42);
        db.insert_account(NONCE_MANAGER_ADDRESS, info);

        ensure_aa_predeploys(&spec, 2, &mut db).unwrap();
        let info = db.basic(NONCE_MANAGER_ADDRESS).unwrap().expect("account should exist");
        assert_eq!(info.balance, U256::from(42));
    }

    #[test]
    fn no_op_when_fork_inactive() {
        let mut db = make_db();
        let spec = base_alloy_upgrades::BaseChainUpgrades::mainnet();

        ensure_aa_predeploys(&spec, 0, &mut db).unwrap();

        let info = db.basic(NONCE_MANAGER_ADDRESS).unwrap();
        assert!(info.is_none());
    }
}
