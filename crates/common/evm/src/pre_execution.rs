//! Pre-execution changes for OP Stack blocks.

use alloy_evm::{
    Database, Evm,
    block::{BlockExecutionError, StateDB, SystemCaller},
};
use alloy_primitives::B256;
use base_alloy_chains::BaseUpgrades;
use revm::DatabaseCommit;

use crate::canyon;

/// Applies standard OP Stack pre-execution changes to the EVM state.
///
/// This includes setting the state clear flag, applying blockhashes and beacon root
/// contract calls, and ensuring the create2 deployer is deployed at the Canyon transition.
#[derive(Debug)]
pub struct BasePreExecution;

impl BasePreExecution {
    /// Applies all pre-execution changes for an OP Stack block.
    ///
    /// This must be called once per block, before executing any transactions.
    /// It performs the following steps:
    /// 1. Sets the state clear flag based on Spurious Dragon activation
    /// 2. Applies the blockhashes contract call (EIP-2935)
    /// 3. Applies the beacon root contract call (EIP-4788)
    /// 4. Ensures the create2 deployer is deployed (Canyon)
    pub fn apply<E, Spec>(
        spec: &Spec,
        system_caller: &mut SystemCaller<Spec>,
        evm: &mut E,
        block_number: u64,
        timestamp: u64,
        parent_hash: B256,
        parent_beacon_block_root: Option<B256>,
    ) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB: Database + DatabaseCommit + StateDB>,
        Spec: BaseUpgrades,
    {
        let state_clear_flag = spec.is_spurious_dragon_active_at_block(block_number);
        evm.db_mut().set_state_clear_flag(state_clear_flag);

        system_caller.apply_blockhashes_contract_call(parent_hash, evm)?;
        system_caller.apply_beacon_root_contract_call(parent_beacon_block_root, evm)?;

        canyon::ensure_create2_deployer(spec, timestamp, evm.db_mut())
            .map_err(BlockExecutionError::other)?;

        Ok(())
    }
}
