use alloy_evm::{
    Database, EvmEnv, EvmFactory,
    precompiles::{DynPrecompile, PrecompilesMap},
};
use alloy_primitives::U256;
use base_revm::{
    BasePrecompiles, DefaultOp, NONCE_MANAGER_ADDRESS, NONCE_MANAGER_GAS, OpBuilder, OpContext,
    OpHaltReason, OpSpecId, OpTransaction, OpTransactionError, TX_CONTEXT_ADDRESS, TX_CONTEXT_GAS,
    aa_nonce_slot, encode_address, encode_b256, encode_u256, get_eip8130_tx_context, selector,
};
use revm::{
    Context, Inspector,
    context::{BlockEnv, TxEnv},
    context_interface::result::EVMError,
    inspector::NoOpInspector,
    precompile::{PrecompileError, PrecompileId, PrecompileOutput},
};

use crate::OpEvm;

fn make_tx_context_precompile() -> DynPrecompile {
    DynPrecompile::new_stateful(PrecompileId::custom("tx_context"), |input| {
        let data = input.data;
        if data.len() < 4 {
            return Err(PrecompileError::Other("invalid tx context input".into()));
        }

        let ctx = get_eip8130_tx_context();
        let (sender, payer, owner_id, gas_limit, max_cost) = match ctx {
            Some(c) => (c.sender, c.payer, c.owner_id.0, c.gas_limit, c.max_cost),
            None => (Default::default(), Default::default(), [0u8; 32], 0, U256::ZERO),
        };

        let sel = &data[0..4];
        let output = if sel == selector(b"getSender()") {
            encode_address(sender)
        } else if sel == selector(b"getPayer()") {
            encode_address(payer)
        } else if sel == selector(b"getOwnerId()") {
            encode_b256(owner_id)
        } else if sel == selector(b"getMaxCost()") {
            encode_u256(max_cost)
        } else if sel == selector(b"getGasLimit()") {
            encode_u256(U256::from(gas_limit))
        } else {
            return Err(PrecompileError::Other("unknown tx context selector".into()));
        };

        if input.gas < TX_CONTEXT_GAS {
            return Err(PrecompileError::OutOfGas);
        }
        Ok(PrecompileOutput::new(TX_CONTEXT_GAS, output))
    })
}

fn make_nonce_manager_precompile() -> DynPrecompile {
    DynPrecompile::new_stateful(PrecompileId::custom("nonce_manager"), |mut input| {
        let data = input.data;
        let get_nonce_sel = selector(b"getNonce(address,uint192)");

        if data.len() < 4 || data[0..4] != get_nonce_sel {
            return Err(PrecompileError::Other("unknown nonce manager selector".into()));
        }
        if data.len() < 4 + 32 + 32 {
            return Err(PrecompileError::Other("invalid nonce manager input".into()));
        }

        let account = alloy_primitives::Address::from_slice(&data[4 + 12..4 + 32]);
        let nonce_key = U256::from_be_slice(&data[4 + 32..4 + 64]);
        let slot = aa_nonce_slot(account, nonce_key);

        let storage_value = input
            .internals
            .sload(NONCE_MANAGER_ADDRESS, slot)
            .map_err(|e| PrecompileError::Other(format!("{e}").into()))?;

        let mut out = [0u8; 32];
        let storage_bytes = storage_value.data.to_be_bytes::<32>();
        out[24..32].copy_from_slice(&storage_bytes[24..32]);

        if input.gas < NONCE_MANAGER_GAS {
            return Err(PrecompileError::OutOfGas);
        }
        Ok(PrecompileOutput::new(
            NONCE_MANAGER_GAS,
            alloy_primitives::Bytes::from(out.to_vec()),
        ))
    })
}

/// Builds a [`PrecompilesMap`] for the given spec, including EIP-8130
/// system precompiles (TxContext, NonceManager) when `BASE_V1` is active.
fn op_precompiles_map(spec: OpSpecId) -> PrecompilesMap {
    let precompiles = OpPrecompiles::new_with_spec(spec);
    let mut map = PrecompilesMap::from_static(precompiles.precompiles());

    if spec == OpSpecId::BASE_V1 {
        map.extend_precompiles([
            (TX_CONTEXT_ADDRESS, make_tx_context_precompile()),
            (NONCE_MANAGER_ADDRESS, make_nonce_manager_precompile()),
        ]);
    }

    map
}

/// Factory producing [`OpEvm`]s.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct OpEvmFactory;

impl EvmFactory for OpEvmFactory {
    type Evm<DB: Database, I: Inspector<OpContext<DB>>> = OpEvm<DB, I, PrecompilesMap>;
    type Context<DB: Database> = OpContext<DB>;
    type Tx = OpTransaction<TxEnv>;
    type Error<DBError: core::error::Error + Send + Sync + 'static> =
        EVMError<DBError, OpTransactionError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let precompiles = op_precompiles_map(input.cfg_env.spec);
        let inner = Context::op()
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .build_op_with_inspector(NoOpInspector {})
            .with_precompiles(precompiles);
        OpEvm { inner, inspect: false }
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let precompiles = op_precompiles_map(input.cfg_env.spec);
        let inner = Context::op()
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .build_op_with_inspector(inspector)
            .with_precompiles(precompiles);
        OpEvm { inner, inspect: true }
    }
}
