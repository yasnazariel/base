use alloy_evm::Database;
use alloy_primitives::{Address, Bytes};
use base_alloy_consensus::{
    ACCOUNT_CONFIG_ADDRESS, DEFAULT_ACCOUNT_ADDRESS, DELEGATE_VERIFIER_ADDRESS,
    K1_VERIFIER_ADDRESS, NONCE_MANAGER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, TX_CONTEXT_ADDRESS,
};
use base_alloy_upgrades::BaseUpgrades;
use revm::{DatabaseCommit, primitives::HashMap, state::Bytecode};

/// All EIP-8130 account-abstraction predeploy addresses.
///
/// These are deployed as part of the Base V1 hardfork activation.
const AA_PREDEPLOY_ADDRESSES: [Address; 8] = [
    ACCOUNT_CONFIG_ADDRESS,
    NONCE_MANAGER_ADDRESS,
    TX_CONTEXT_ADDRESS,
    DEFAULT_ACCOUNT_ADDRESS,
    K1_VERIFIER_ADDRESS,
    P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS,
    DELEGATE_VERIFIER_ADDRESS,
];

/// Stub bytecode deployed to each AA predeploy address.
///
/// `0xFE` is the `INVALID` opcode -- any direct call reverts immediately.
/// The real contract logic is handled by the node as native precompiles
/// in the EVM handler. This stub ensures the accounts are non-empty
/// under EIP-161, preventing state cleanup from deleting their storage.
const AA_STUB_BYTECODE: &[u8] = &[0xFE];

/// The Base V1 hardfork issues an irregular state transition that force-deploys
/// stub bytecode to all EIP-8130 account-abstraction predeploy addresses.
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
    // address (AccountConfig) still has no code. The second check
    // handles genesis-activated devnets where the first-block
    // heuristic (`timestamp - 2`) can't distinguish block 0 from
    // block 1.
    let sentinel = db.basic(ACCOUNT_CONFIG_ADDRESS)?;
    let already_deployed =
        sentinel.as_ref().is_some_and(|info| info.code_hash != revm::primitives::KECCAK_EMPTY);

    if already_deployed {
        return Ok(());
    }

    let code = Bytecode::new_raw(Bytes::from_static(AA_STUB_BYTECODE));
    let code_hash = code.hash_slow();

    let mut accounts = HashMap::default();
    for addr in AA_PREDEPLOY_ADDRESSES {
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
    fn deploys_all_predeploys_on_activation() {
        let mut db = make_db();
        let spec = devnet_spec();

        ensure_aa_predeploys(&spec, 0, &mut db).unwrap();

        for addr in AA_PREDEPLOY_ADDRESSES {
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

        // Set a balance on one account
        let mut info = db.basic(NONCE_MANAGER_ADDRESS).unwrap().unwrap_or_default();
        info.balance = U256::from(42);
        db.insert_account(NONCE_MANAGER_ADDRESS, info);

        // Second call is a no-op because sentinel already has code
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
