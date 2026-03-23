use core::ops::{Deref, DerefMut};

use alloy_evm::{Database, Evm, EvmEnv};
use alloy_primitives::{Address, Bytes};
use base_revm::{
    BasePrecompiles, OpContext, OpHaltReason, OpSpecId, OpTransaction, OpTransactionError,
};
use revm::{
    ExecuteEvm, InspectEvm, Inspector, SystemCallEvm,
    context::{BlockEnv, TxEnv},
    context_interface::result::{EVMError, ResultAndState},
    handler::{PrecompileProvider, instructions::EthInstructions},
    interpreter::{InterpreterResult, interpreter::EthInterpreter},
};

/// OP EVM implementation.
///
/// This is a wrapper type around the `revm` evm with optional [`Inspector`] (tracing)
/// support. [`Inspector`] support is configurable at runtime because it's part of the underlying
/// [`OpEvm`](base_revm::OpEvm) type.
#[allow(missing_debug_implementations)] // missing revm::OpContext Debug impl
pub struct OpEvm<DB: Database, I, P = BasePrecompiles> {
    pub(crate) inner:
        base_revm::OpEvm<OpContext<DB>, I, EthInstructions<EthInterpreter, OpContext<DB>>, P>,
    pub(crate) inspect: bool,
}

impl<DB: Database, I, P> OpEvm<DB, I, P> {
    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &OpContext<DB> {
        &self.inner.0.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub const fn ctx_mut(&mut self) -> &mut OpContext<DB> {
        &mut self.inner.0.ctx
    }
}

impl<DB: Database, I, P> OpEvm<DB, I, P> {
    /// Creates a new OP EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`OpEvm`](base_revm::OpEvm) should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: base_revm::OpEvm<OpContext<DB>, I, EthInstructions<EthInterpreter, OpContext<DB>>, P>,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }
}

impl<DB: Database, I, P> Deref for OpEvm<DB, I, P> {
    type Target = OpContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, I, P> DerefMut for OpEvm<DB, I, P> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, P> Evm for OpEvm<DB, I, P>
where
    DB: Database,
    I: Inspector<OpContext<DB>>,
    P: PrecompileProvider<OpContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = OpTransaction<TxEnv>;
    type Error = EVMError<DB::Error, OpTransactionError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = P;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.block
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let result = if self.inspect {
            self.inner.inspect_one_tx(tx)?
        } else {
            self.inner.transact_one(tx)?
        };
        let state = self.inner.finalize();
        Ok(ResultAndState::new(result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let result = self.inner.system_call_one_with_caller(caller, contract, data)?;
        let state = self.inner.finalize();
        Ok(ResultAndState::new(result, state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let revm::Context { block: block_env, cfg: cfg_env, journaled_state, .. } =
            self.inner.0.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.0.ctx.journaled_state.database,
            &self.inner.0.inspector,
            &self.inner.0.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.0.ctx.journaled_state.database,
            &mut self.inner.0.inspector,
            &mut self.inner.0.precompiles,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use alloy_evm::{
        Evm, EvmFactory, EvmInternals,
        precompiles::{Precompile, PrecompileInput},
    };
    use alloy_primitives::{Address, Bytes, U256, bytes};
    use base_revm::{Eip8130Call, Eip8130Parts, OpTransaction, bls12_381, bn254_pair, decode_phase_statuses};
    use revm::{
        bytecode::Bytecode,
        context::{CfgEnv, TxEnv},
        database::{EmptyDB, InMemoryDB},
        primitives::TxKind,
        state::AccountInfo,
    };
    use rstest::rstest;

    use super::*;
    use crate::OpEvmFactory;

    #[rstest]
    #[case::bn254_pair(*bn254_pair::JOVIAN.address(), bn254_pair::JOVIAN_MAX_INPUT_SIZE)]
    #[case::bls12_g1_msm(*bls12_381::JOVIAN_G1_MSM.address(), bls12_381::JOVIAN_G1_MSM_MAX_INPUT_SIZE)]
    #[case::bls12_g2_msm(*bls12_381::JOVIAN_G2_MSM.address(), bls12_381::JOVIAN_G2_MSM_MAX_INPUT_SIZE)]
    #[case::bls12_pairing(*bls12_381::JOVIAN_PAIRING.address(), bls12_381::JOVIAN_PAIRING_MAX_INPUT_SIZE)]
    fn precompile_jovian_at_max_input(#[case] address: Address, #[case] max_size: usize) {
        let mut evm = OpEvmFactory::default().create_evm(
            EmptyDB::default(),
            EvmEnv::new(CfgEnv::new_with_spec(OpSpecId::JOVIAN), BlockEnv::default()),
        );
        let (precompiles, ctx) = (&mut evm.inner.0.precompiles, &mut evm.inner.0.ctx);
        let precompile = precompiles.get(&address).unwrap();
        let result = precompile.call(PrecompileInput {
            data: &vec![0; max_size],
            gas: u64::MAX,
            caller: Address::ZERO,
            value: U256::ZERO,
            is_static: false,
            target_address: Address::ZERO,
            bytecode_address: Address::ZERO,
            internals: EvmInternals::from_context(ctx),
        });
        assert!(result.is_ok(), "precompile {address} should succeed at max input size");
    }

    #[rstest]
    #[case::bn254_pair(*bn254_pair::JOVIAN.address(), bn254_pair::JOVIAN_MAX_INPUT_SIZE)]
    #[case::bls12_g1_msm(*bls12_381::JOVIAN_G1_MSM.address(), bls12_381::JOVIAN_G1_MSM_MAX_INPUT_SIZE)]
    #[case::bls12_g2_msm(*bls12_381::JOVIAN_G2_MSM.address(), bls12_381::JOVIAN_G2_MSM_MAX_INPUT_SIZE)]
    #[case::bls12_pairing(*bls12_381::JOVIAN_PAIRING.address(), bls12_381::JOVIAN_PAIRING_MAX_INPUT_SIZE)]
    fn precompile_jovian_over_max_input(#[case] address: Address, #[case] max_size: usize) {
        let mut evm = OpEvmFactory::default().create_evm(
            EmptyDB::default(),
            EvmEnv::new(CfgEnv::new_with_spec(OpSpecId::JOVIAN), BlockEnv::default()),
        );
        let (precompiles, ctx) = (&mut evm.inner.0.precompiles, &mut evm.inner.0.ctx);
        let precompile = precompiles.get(&address).unwrap();
        let result = precompile.call(PrecompileInput {
            data: &vec![0; max_size + 1],
            gas: u64::MAX,
            caller: Address::ZERO,
            value: U256::ZERO,
            is_static: false,
            target_address: Address::ZERO,
            bytecode_address: Address::ZERO,
            internals: EvmInternals::from_context(ctx),
        });
        assert!(result.is_err(), "precompile {address} should fail over max input size");
    }

    #[test]
    fn transact_raw_uses_op_handler_for_eip8130() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            target,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("00"))), ..Default::default() },
        );

        let mut evm = OpEvmFactory::default().create_evm(
            db,
            EvmEnv::new(CfgEnv::new_with_spec(OpSpecId::BASE_V1), BlockEnv::default()),
        );

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(100_000)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = Eip8130Parts {
            sender,
            payer: sender,
            call_phases: vec![vec![Eip8130Call { to: target, data: Bytes::new(), value: U256::ZERO }]],
            ..Default::default()
        };

        let result = evm.transact_raw(tx).expect("EIP-8130 tx should execute");
        assert!(result.result.is_success(), "AA phase execution should succeed");
        let statuses =
            decode_phase_statuses(result.result.output().expect("AA tx should return phase status"));
        assert_eq!(statuses, vec![true]);
    }
}
