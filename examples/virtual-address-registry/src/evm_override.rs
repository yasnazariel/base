//! Custom EVM override that adds the virtual address registry precompile.

use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_sol_types::SolCall;
use base_action_harness::{
    EvmOverride, L2SequencerError, StatefulL2Executor, TEST_ACCOUNT_ADDRESS, compute_state_root,
};
use base_address_resolution::REGISTRY_ADDRESS;
use base_alloy_consensus::OpTxEnvelope;
use base_consensus_genesis::RollupConfig;
use revm::database::InMemoryDB;

use crate::{StorageBackedRegistry, abi::IAddressRegistry};

/// EVM override that intercepts calls to the virtual address registry.
///
/// When a transaction targets [`REGISTRY_ADDRESS`], this override decodes the
/// ABI calldata and executes the registry operation directly against the EVM
/// database. All other transactions are executed normally via the default
/// `OpEvmConfig::optimism()` path.
#[derive(Debug)]
pub struct RegistryEvmOverride;

impl EvmOverride for RegistryEvmOverride {
    fn execute_transactions(
        &self,
        db: &mut InMemoryDB,
        rollup_config: &RollupConfig,
        transactions: &[OpTxEnvelope],
        block_number: u64,
        timestamp: u64,
        parent_hash: B256,
    ) -> Result<(B256, u64), L2SequencerError> {
        // Split transactions: registry calls are handled here, everything else
        // is delegated to the default execution path.
        let mut non_registry_txs: Vec<OpTxEnvelope> = Vec::new();
        let mut registry_gas = 0u64;

        for tx in transactions {
            if is_registry_call(tx) {
                let sender = tx_sender(tx);
                let input = tx_input(tx);
                let gas = Self::handle_registry_call(db, sender, &input);
                registry_gas = registry_gas.saturating_add(gas);

                // Advance the sender's nonce and debit gas cost.
                if let Some(acct) = db.cache.accounts.get_mut(&sender) {
                    acct.info.nonce += 1;
                    acct.info.balance = acct
                        .info
                        .balance
                        .saturating_sub(U256::from(gas) * U256::from(1_000_000_000u64));
                }
            } else {
                non_registry_txs.push(tx.clone());
            }
        }

        // Execute non-registry transactions via the default path.
        let (_state_root, default_gas) = StatefulL2Executor::default_execute_transactions(
            db,
            rollup_config,
            &non_registry_txs,
            block_number,
            timestamp,
            parent_hash,
        )?;

        // The state root from default_execute_transactions already accounts for
        // the registry writes we made above (they're in the db). But gas needs
        // to be combined.
        //
        // Note: recompute state root to include registry writes that happened
        // before the default execution.
        let final_root = compute_state_root(db);

        Ok((final_root, default_gas.saturating_add(registry_gas)))
    }
}

impl RegistryEvmOverride {
    /// Handle a call to the registry address by decoding the ABI selector and
    /// executing the appropriate registry operation.
    ///
    /// Returns the gas consumed.
    fn handle_registry_call(db: &mut InMemoryDB, caller: Address, input: &[u8]) -> u64 {
        const REGISTRY_GAS: u64 = 50_000;

        if input.len() < 4 {
            return REGISTRY_GAS;
        }

        let selector: [u8; 4] = input[..4].try_into().unwrap();

        if selector == IAddressRegistry::registerVirtualMasterCall::SELECTOR
            && let Ok(call) =
                <IAddressRegistry::registerVirtualMasterCall as SolCall>::abi_decode(&input[4..])
            {
                let _ = StorageBackedRegistry::register(db, caller, B256::from(call.salt));
            }
        // Other selectors (resolveRecipient, getMaster, etc.) are read-only and
        // don't need special handling in this simplified example.

        REGISTRY_GAS
    }
}

/// Check if a transaction targets the registry address.
fn is_registry_call(tx: &OpTxEnvelope) -> bool {
    match tx {
        OpTxEnvelope::Eip1559(signed) => signed.tx().to == TxKind::Call(REGISTRY_ADDRESS),
        _ => false,
    }
}

/// Extract the input (calldata) from a transaction.
fn tx_input(tx: &OpTxEnvelope) -> Bytes {
    match tx {
        OpTxEnvelope::Eip1559(signed) => signed.tx().input.clone(),
        _ => Bytes::new(),
    }
}

/// Determine the sender address for a transaction.
const fn tx_sender(tx: &OpTxEnvelope) -> Address {
    match tx {
        OpTxEnvelope::Deposit(sealed) => sealed.inner().from,
        _ => TEST_ACCOUNT_ADDRESS,
    }
}
