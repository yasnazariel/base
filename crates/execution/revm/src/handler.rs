//! Handler related to Base chain
use std::boxed::Box;

use revm::{
    context::{
        LocalContextTr,
        journaled_state::{JournalCheckpoint, account::JournaledAccountTr},
        result::InvalidTransaction,
    },
    context_interface::{
        Block, Cfg, ContextTr, JournalTr, Transaction,
        context::ContextError,
        result::{EVMError, ExecutionResult, FromStringError},
    },
    handler::{
        EthFrame, EvmTr, FrameResult, Handler, MainnetHandler,
        evm::FrameTr,
        handler::EvmTrError,
        post_execution::{self, reimburse_caller},
        pre_execution::{calculate_caller_fee, validate_account_nonce_and_code_with_components},
    },
    inspector::{Inspector, InspectorEvmTr, InspectorHandler},
    interpreter::{
        CallOutcome, Gas, InitialAndFloorGas, InstructionResult, InterpreterResult, SharedMemory,
        interpreter::EthInterpreter,
        interpreter_action::{CallInput, CallInputs, CallScheme, CallValue, FrameInit, FrameInput},
    },
    primitives::{Address, U256, hardfork::SpecId, keccak256},
};

use crate::{
    Eip8130PhaseResult, Eip8130TxContext, L1BlockInfo, NONCE_MANAGER_ADDRESS, OpContextTr,
    OpHaltReason, OpSpecId, TX_CONTEXT_ADDRESS, clear_eip8130_tx_context,
    constants::{BASE_FEE_RECIPIENT, L1_FEE_RECIPIENT, OPERATOR_FEE_RECIPIENT},
    encode_phase_statuses, phase_statuses_system_log, set_eip8130_tx_context,
    transaction::{DEPOSIT_TRANSACTION_TYPE, OpTransactionError, OpTxTr},
};

/// EIP-8130 AA transaction type byte.
const EIP8130_TX_TYPE: u8 = 0x05;

/// AccountConfiguration deployed contract address.
/// Must match the CREATE2 address from `Deploy.s.sol` (salt = 0).
const ACCOUNT_CONFIG_ADDRESS: Address = Address::new([
    0x0F, 0x12, 0x71, 0x93, 0xb7, 0x2E, 0x0f, 0x85, 0x46, 0xA6, 0xF4, 0xE4, 0x71, 0xb6, 0xF8,
    0x24, 0x19, 0x00, 0x93, 0x2B,
]);

/// Base storage slot for the nonce mapping in NonceManager (slot index 1).
const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Computes the NonceManager storage slot for `nonce[account][nonce_key]`.
///
/// `keccak256(nonce_key . keccak256(account . NONCE_BASE_SLOT))`
///
/// Mirrors [`base_alloy_consensus::nonce_slot`] to avoid a cyclic dependency.
fn aa_nonce_slot(account: Address, nonce_key: U256) -> U256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        let base_bytes = NONCE_BASE_SLOT.to_be_bytes::<32>();
        buf[32..64].copy_from_slice(&base_bytes);
        keccak256(buf)
    };
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(&nonce_key.to_be_bytes::<32>());
    buf[32..64].copy_from_slice(inner.as_slice());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Owner config base storage slot in AccountConfig (slot index 0).
///
/// `keccak256(owner_id . keccak256(account . 0))`
const OWNER_CONFIG_BASE_SLOT: U256 = U256::ZERO;

/// Computes the AccountConfig storage slot for `owner_config(account, owner_id)`.
///
/// Mirrors [`base_alloy_consensus::owner_config_slot`] to avoid a cyclic dependency.
fn aa_owner_config_slot(account: Address, owner_id: U256) -> U256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        let base_bytes = OWNER_CONFIG_BASE_SLOT.to_be_bytes::<32>();
        buf[32..64].copy_from_slice(&base_bytes);
        keccak256(buf)
    };
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(&owner_id.to_be_bytes::<32>());
    buf[32..64].copy_from_slice(inner.as_slice());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Parses a packed owner_config word into `(verifier_address, scope)`.
///
/// Layout: `[scope(1) | zeros(11) | verifier(20)]` (big-endian 32 bytes).
fn parse_owner_config_word(word: U256) -> (Address, u8) {
    let bytes = word.to_be_bytes::<32>();
    let scope = bytes[0];
    let verifier = Address::from_slice(&bytes[12..32]);
    (verifier, scope)
}

/// Base handler extends the [`Handler`] with Base-specific logic.
#[derive(Debug, Clone)]
pub struct OpHandler<EVM, ERROR, FRAME> {
    /// Mainnet handler allows us to use functions from the mainnet handler inside the Base handler.
    /// So we dont duplicate the logic
    pub mainnet: MainnetHandler<EVM, ERROR, FRAME>,
}

impl<EVM, ERROR, FRAME> OpHandler<EVM, ERROR, FRAME> {
    /// Create a new Base handler.
    pub fn new() -> Self {
        Self { mainnet: MainnetHandler::default() }
    }
}

impl<EVM, ERROR, FRAME> Default for OpHandler<EVM, ERROR, FRAME> {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait to check if the error is a transaction error.
///
/// Used in `cache_error` handler to catch deposit transaction that was halted.
pub trait IsTxError {
    /// Check if the error is a transaction error.
    fn is_tx_error(&self) -> bool;
}

impl<DB, TX> IsTxError for EVMError<DB, TX> {
    fn is_tx_error(&self) -> bool {
        matches!(self, Self::Transaction(_))
    }
}

impl<EVM, ERROR, FRAME> Handler for OpHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: OpContextTr, Frame = FRAME>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
    // TODO `FrameResult` should be a generic trait.
    // TODO `FrameInit` should be a generic.
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = OpHaltReason;

    fn validate_env(&self, evm: &mut Self::Evm) -> Result<(), Self::Error> {
        // Do not perform any extra validation for deposit transactions, they are pre-verified on L1.
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let tx_type = tx.tx_type();
        if tx_type == DEPOSIT_TRANSACTION_TYPE {
            // Do not allow for a system transaction to be processed if Regolith is enabled.
            if tx.is_system_transaction()
                && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH)
            {
                return Err(OpTransactionError::DepositSystemTxPostRegolith.into());
            }
            return Ok(());
        }

        // Check that non-deposit transactions have enveloped_tx set
        if tx.enveloped_tx().is_none() {
            return Err(OpTransactionError::MissingEnvelopedTx.into());
        }

        // AA transactions require BASE_V1. Reject if the spec is not active.
        if tx_type == EIP8130_TX_TYPE {
            if !evm.ctx().cfg().spec().is_enabled_in(OpSpecId::BASE_V1) {
                return Err(OpTransactionError::Base(
                    InvalidTransaction::Str("EIP-8130 AA transactions require BASE_V1".into()),
                )
                .into());
            }

            let ctx = evm.ctx();
            let basefee = ctx.block().basefee() as u128;
            let max_fee = ctx.tx().max_fee_per_gas();
            let max_priority = ctx.tx().max_priority_fee_per_gas().unwrap_or(0);

            if max_fee < basefee {
                return Err(OpTransactionError::Base(
                    InvalidTransaction::Str(
                        "EIP-8130: max_fee_per_gas below base fee".into(),
                    ),
                )
                .into());
            }
            if max_priority > max_fee {
                return Err(OpTransactionError::Base(
                    InvalidTransaction::Str(
                        "EIP-8130: max_priority_fee_per_gas exceeds max_fee_per_gas".into(),
                    ),
                )
                .into());
            }

            return Ok(());
        }

        self.mainnet.validate_env(evm)
    }

    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<InitialAndFloorGas, Self::Error> {
        if evm.ctx().tx().tx_type() == EIP8130_TX_TYPE {
            let aa_gas = evm.ctx().tx().eip8130_parts().aa_intrinsic_gas;
            if aa_gas > evm.ctx().tx().gas_limit() {
                return Err(InvalidTransaction::CallGasCostMoreThanGasLimit {
                    gas_limit: evm.ctx().tx().gas_limit(),
                    initial_gas: aa_gas,
                }
                .into());
            }
            return Ok(InitialAndFloorGas::new(aa_gas, 0));
        }
        self.mainnet.validate_initial_tx_gas(evm)
    }

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        let (block, tx, cfg, journal, chain, _) = evm.ctx().all_mut();
        let spec = cfg.spec();

        if tx.tx_type() == DEPOSIT_TRANSACTION_TYPE {
            let basefee = block.basefee() as u128;
            let blob_price = block.blob_gasprice().unwrap_or_default();
            // deposit skips max fee check and just deducts the effective balance spending.

            let mut caller = journal.load_account_with_code_mut(tx.caller())?.data;

            let effective_balance_spending = tx
                .effective_balance_spending(basefee, blob_price)
                .expect("Deposit transaction effective balance spending overflow")
                - tx.value();

            // Mind value should be added first before subtracting the effective balance spending.
            let mut new_balance = caller
                .balance()
                .saturating_add(U256::from(tx.mint().unwrap_or_default()))
                .saturating_sub(effective_balance_spending);

            if cfg.is_balance_check_disabled() {
                // Make sure the caller's balance is at least the value of the transaction.
                // this is not consensus critical, and it is used in testing.
                new_balance = new_balance.max(tx.value());
            }

            // set the new balance and bump the nonce if it is a call
            caller.set_balance(new_balance);
            if tx.kind().is_call() {
                caller.bump_nonce();
            }

            return Ok(());
        }

        // L1 block info is stored in the context for later use.
        // and it will be reloaded from the database if it is not for the current block.
        if chain.l2_block != Some(block.number()) {
            *chain = L1BlockInfo::try_fetch(journal.db_mut(), block.number(), spec)?;
        }

        // Clear any stale EIP-8130 context from a previous transaction.
        clear_eip8130_tx_context();

        // AA transactions: deduct gas from payer, increment NonceManager nonce,
        // auto-delegate bare EOAs, and apply pre-execution storage writes.
        if tx.tx_type() == EIP8130_TX_TYPE {
            let sender = tx.caller();
            let nonce_sequence = tx.nonce();
            let eip8130 = tx.eip8130_parts().clone();

            set_eip8130_tx_context(Eip8130TxContext::from((
                &eip8130,
                tx.gas_limit(),
                U256::from(tx.max_fee_per_gas()),
            )));

            // --- Gas deduction from payer ---
            let payer = eip8130.payer;
            let mut payer_account = journal.load_account_with_code_mut(payer)?.data;
            let mut balance = payer_account.account().info.balance;

            if !cfg.is_fee_charge_disabled() {
                let Some(additional_cost) = chain.tx_cost_with_tx(tx, spec) else {
                    return Err(ERROR::from_string(
                        "[OPTIMISM] Failed to load enveloped transaction.".into(),
                    ));
                };
                let Some(new_balance) = balance.checked_sub(additional_cost) else {
                    return Err(InvalidTransaction::LackOfFundForMaxFee {
                        fee: Box::new(additional_cost),
                        balance: Box::new(balance),
                    }
                    .into());
                };
                balance = new_balance;
            }

            let balance = calculate_caller_fee(balance, tx, block, cfg)?;
            payer_account.set_balance(balance);
            drop(payer_account);

            // Check if sender is a bare EOA (no code) for auto-delegation.
            let sender_account = journal.load_account_with_code_mut(sender)?.data;
            let sender_has_code = sender_account.account().info.code_hash != keccak256([]);
            drop(sender_account);

            // --- Nonce increment in NonceManager ---
            let nonce_key = eip8130.nonce_key;
            let slot = aa_nonce_slot(sender, nonce_key);
            let new_nonce = U256::from(nonce_sequence + 1);

            journal.load_account(NONCE_MANAGER_ADDRESS)?;
            journal.sstore(NONCE_MANAGER_ADDRESS, slot, new_nonce)?;

            // --- Auto-delegate bare EOAs ---
            // If sender has no code and there's no create entry deploying new
            // code, set EIP-7702 delegation designator pointing at the
            // DEFAULT_ACCOUNT_ADDRESS.
            if !sender_has_code
                && !eip8130.has_create_entry
                && !eip8130.auto_delegation_code.is_empty()
            {
                let code = revm::bytecode::Bytecode::new_raw(eip8130.auto_delegation_code.clone());
                let mut acc = journal.load_account_with_code_mut(sender)?.data;
                acc.set_code_and_hash_slow(code);
                drop(acc);
            }

            // --- Apply pre-execution storage writes ---
            // Owner registrations, config changes.
            for w in &eip8130.pre_writes {
                journal.load_account(w.address)?;
                journal.sstore(w.address, w.slot, w.value)?;
            }

            // --- Account creation (place runtime bytecode at CREATE2-derived addresses) ---
            for placement in &eip8130.code_placements {
                let code =
                    revm::bytecode::Bytecode::new_raw(placement.code.clone());
                let mut acc = journal.load_account_with_code_mut(placement.address)?.data;
                acc.set_code_and_hash_slow(code);
                drop(acc);
            }

            // --- Sequence bumps (read-modify-write on packed ChangeSequences slot) ---
            if !eip8130.sequence_updates.is_empty() {
                journal.load_account(ACCOUNT_CONFIG_ADDRESS)?;
                for upd in &eip8130.sequence_updates {
                    let current = journal.sload(ACCOUNT_CONFIG_ADDRESS, upd.slot)?.data;
                    let new_packed = upd.apply(current);
                    journal.sstore(ACCOUNT_CONFIG_ADDRESS, upd.slot, new_packed)?;
                }
            }

            return Ok(());
        }

        let mut caller_account = journal.load_account_with_code_mut(tx.caller())?.data;

        // validates account nonce and code
        validate_account_nonce_and_code_with_components(&caller_account.account().info, tx, cfg)?;

        // check additional cost and deduct it from the caller's balances
        let mut balance = caller_account.account().info.balance;

        if !cfg.is_fee_charge_disabled() {
            let Some(additional_cost) = chain.tx_cost_with_tx(tx, spec) else {
                return Err(ERROR::from_string(
                    "[OPTIMISM] Failed to load enveloped transaction.".into(),
                ));
            };
            let Some(new_balance) = balance.checked_sub(additional_cost) else {
                return Err(InvalidTransaction::LackOfFundForMaxFee {
                    fee: Box::new(additional_cost),
                    balance: Box::new(balance),
                }
                .into());
            };
            balance = new_balance
        }

        let balance = calculate_caller_fee(balance, tx, block, cfg)?;

        // make changes to the account
        caller_account.set_balance(balance);
        if tx.kind().is_call() {
            caller_account.bump_nonce();
        }

        Ok(())
    }

    fn execution(
        &mut self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &InitialAndFloorGas,
    ) -> Result<FrameResult, Self::Error> {
        if evm.ctx().tx().tx_type() != EIP8130_TX_TYPE {
            return self.mainnet.execution(evm, init_and_floor_gas);
        }

        let gas_limit = evm.ctx().tx().gas_limit().saturating_sub(init_and_floor_gas.initial_gas);
        let eip8130 = evm.ctx().tx().eip8130_parts().clone();
        let sender = evm.ctx().tx().caller();

        let mut gas_remaining = gas_limit;
        let mut phase_results = Vec::with_capacity(eip8130.call_phases.len());

        // Ensure sender is loaded in the journal state for sub-call transfers.
        evm.ctx().journal_mut().load_account(sender)?;

        // --- Custom verifier STATICCALL verification ---
        // When a custom verifier is used (type 0x00), we STATICCALL the verifier
        // contract on-chain to get the authenticated owner_id, then validate it
        // against the on-chain owner_config in AccountConfig.
        let verify_calls = [
            eip8130.sender_verify_call.as_ref(),
            eip8130.payer_verify_call.as_ref(),
        ];
        for verify_call in verify_calls.into_iter().flatten() {
            evm.ctx().journal_mut().load_account(verify_call.verifier)?;

            let call_gas = gas_remaining;
            let call_inputs = CallInputs {
                input: CallInput::Bytes(verify_call.calldata.clone()),
                return_memory_offset: 0..0,
                gas_limit: call_gas,
                bytecode_address: verify_call.verifier,
                known_bytecode: None,
                target_address: verify_call.verifier,
                caller: sender,
                value: CallValue::Transfer(U256::ZERO),
                scheme: CallScheme::StaticCall,
                is_static: true,
            };

            let frame_init = FrameInit {
                depth: 0,
                memory: {
                    let ctx = evm.ctx();
                    let mut mem =
                        SharedMemory::new_with_buffer(ctx.local().shared_memory_buffer().clone());
                    mem.set_memory_limit(ctx.cfg().memory_limit());
                    mem
                },
                frame_input: FrameInput::Call(Box::new(call_inputs)),
            };

            let result = self.mainnet.run_exec_loop(evm, frame_init)?;
            let used = call_gas.saturating_sub(result.gas().remaining());
            gas_remaining = gas_remaining.saturating_sub(used);

            if !result.interpreter_result().result.is_ok() {
                return Err(ERROR::from_string(
                    "custom verifier STATICCALL failed".into(),
                ));
            }

            let output = result.interpreter_result().output.as_ref();
            if output.len() < 32 {
                return Err(ERROR::from_string(
                    "custom verifier returned invalid owner_id (< 32 bytes)".into(),
                ));
            }
            let mut owner_id_bytes = [0u8; 32];
            owner_id_bytes.copy_from_slice(&output[..32]);
            let owner_id = U256::from_be_bytes(owner_id_bytes);

            evm.ctx().journal_mut().load_account(ACCOUNT_CONFIG_ADDRESS)?;
            let slot = aa_owner_config_slot(verify_call.account, owner_id);
            let config_word =
                evm.ctx().journal_mut().sload(ACCOUNT_CONFIG_ADDRESS, slot)?.data;
            let (on_chain_verifier, scope) = parse_owner_config_word(config_word);

            if on_chain_verifier != verify_call.verifier {
                return Err(ERROR::from_string(format!(
                    "custom verifier mismatch: expected {}, got {}",
                    verify_call.verifier, on_chain_verifier
                )));
            }
            if scope != 0 && (scope & verify_call.required_scope) == 0 {
                return Err(ERROR::from_string(format!(
                    "owner lacks required scope bit 0x{:02x}",
                    verify_call.required_scope
                )));
            }
        }

        for phase in &eip8130.call_phases {
            let checkpoint = evm.ctx().journal_mut().checkpoint();
            let mut phase_ok = true;
            let phase_gas_start = gas_remaining;

            for call in phase {
                if gas_remaining == 0 {
                    phase_ok = false;
                    break;
                }

                evm.ctx().journal_mut().load_account(call.to)?;

                let call_gas = gas_remaining;
                let call_inputs = CallInputs {
                    input: CallInput::Bytes(call.data.clone()),
                    return_memory_offset: 0..0,
                    gas_limit: call_gas,
                    bytecode_address: call.to,
                    known_bytecode: None,
                    target_address: call.to,
                    caller: sender,
                    value: CallValue::Transfer(call.value),
                    scheme: CallScheme::Call,
                    is_static: false,
                };

                let frame_init = FrameInit {
                    depth: 0,
                    memory: {
                        let ctx = evm.ctx();
                        let mut mem = SharedMemory::new_with_buffer(
                            ctx.local().shared_memory_buffer().clone(),
                        );
                        mem.set_memory_limit(ctx.cfg().memory_limit());
                        mem
                    },
                    frame_input: FrameInput::Call(Box::new(call_inputs)),
                };

                let call_result = self.mainnet.run_exec_loop(evm, frame_init)?;
                let call_gas_used = call_gas.saturating_sub(call_result.gas().remaining());
                gas_remaining = gas_remaining.saturating_sub(call_gas_used);

                if !call_result.interpreter_result().result.is_ok() {
                    phase_ok = false;
                    break;
                }
            }

            if !phase_ok {
                evm.ctx().journal_mut().checkpoint_revert(checkpoint);
            }

            phase_results.push(Eip8130PhaseResult {
                success: phase_ok,
                gas_used: phase_gas_start.saturating_sub(gas_remaining),
            });
        }

        let any_phase_succeeded = phase_results.iter().any(|r| r.success);

        // Deploy-only transactions (account creation with no call phases) succeed
        // when pre-execution code placement completed without error.
        let deploy_only_success =
            phase_results.is_empty() && !eip8130.code_placements.is_empty();

        let tx_succeeded = any_phase_succeeded || deploy_only_success;

        // Emit a system log with per-phase statuses so they survive in the receipt's
        // log list and can be recovered at RPC time. Always emitted when phases
        // exist so `extract_phase_statuses_from_logs` returns authoritative data
        // regardless of the tx outcome.
        if !phase_results.is_empty() {
            evm.ctx()
                .journal_mut()
                .log(phase_statuses_system_log(TX_CONTEXT_ADDRESS, &phase_results));
        }

        let mut result_gas = Gas::new_spent(evm.ctx().tx().gas_limit());
        result_gas.erase_cost(gas_remaining);

        let output = encode_phase_statuses(&phase_results);

        let instruction_result =
            if tx_succeeded { InstructionResult::Stop } else { InstructionResult::Revert };

        let mut frame_result = FrameResult::Call(CallOutcome::new(
            InterpreterResult { result: instruction_result, output, gas: result_gas },
            0..0,
        ));

        self.last_frame_result(evm, &mut frame_result)?;
        Ok(frame_result)
    }

    fn last_frame_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let tx_gas_limit = tx.gas_limit();
        let is_regolith = ctx.cfg().spec().is_enabled_in(OpSpecId::REGOLITH);

        let instruction_result = frame_result.interpreter_result().result;
        let gas = frame_result.gas_mut();
        let remaining = gas.remaining();
        let refunded = gas.refunded();

        // Spend the gas limit. Gas is reimbursed when the tx returns successfully.
        *gas = Gas::new_spent(tx_gas_limit);

        if instruction_result.is_ok() {
            if !is_deposit || is_regolith {
                gas.erase_cost(remaining);
                gas.record_refund(refunded);
            } else if is_deposit && tx.is_system_transaction() {
                gas.erase_cost(tx_gas_limit);
            }
        } else if instruction_result.is_revert() && (!is_deposit || is_regolith) {
            gas.erase_cost(remaining);
        }
        Ok(())
    }

    fn reimburse_caller(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let mut additional_refund = U256::ZERO;

        if evm.ctx().tx().tx_type() != DEPOSIT_TRANSACTION_TYPE
            && !evm.ctx().cfg().is_fee_charge_disabled()
        {
            let spec = evm.ctx().cfg().spec();
            additional_refund = evm.ctx().chain().operator_fee_refund(frame_result.gas(), spec);
        }

        // For EIP-8130 sponsored transactions, refund the payer (not tx.caller()).
        if evm.ctx().tx().tx_type() == EIP8130_TX_TYPE {
            let payer = evm.ctx().tx().eip8130_parts().payer;
            let basefee = evm.ctx().block().basefee() as u128;
            let effective_gas_price = evm.ctx().tx().effective_gas_price(basefee);
            let gas = frame_result.gas();
            let refund_amount = U256::from(
                effective_gas_price
                    .saturating_mul((gas.remaining() + gas.refunded() as u64) as u128),
            ) + additional_refund;
            evm.ctx().journal_mut().load_account_mut(payer)?.incr_balance(refund_amount);
            return Ok(());
        }

        reimburse_caller(evm.ctx(), frame_result.gas(), additional_refund).map_err(From::from)
    }

    fn refund(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
        eip7702_refund: i64,
    ) {
        frame_result.gas_mut().record_refund(eip7702_refund);

        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_regolith = evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH);

        // Prior to Regolith, deposit transactions did not receive gas refunds.
        let is_gas_refund_disabled = is_deposit && !is_regolith;
        if !is_gas_refund_disabled {
            frame_result.gas_mut().set_final_refund(
                evm.ctx().cfg().spec().into_eth_spec().is_enabled_in(SpecId::LONDON),
            );
        }
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;

        // Transfer fee to coinbase/beneficiary.
        if is_deposit {
            return Ok(());
        }

        self.mainnet.reward_beneficiary(evm, frame_result)?;
        let basefee = evm.ctx().block().basefee() as u128;

        let ctx = evm.ctx();
        let enveloped = ctx.tx().enveloped_tx().cloned();
        let spec = ctx.cfg().spec();
        let l1_block_info = ctx.chain_mut();

        let Some(enveloped_tx) = &enveloped else {
            return Err(ERROR::from_string(
                "[OPTIMISM] Failed to load enveloped transaction.".into(),
            ));
        };

        let l1_cost = l1_block_info.calculate_tx_l1_cost(enveloped_tx, spec);
        let operator_fee_cost = if spec.is_enabled_in(OpSpecId::ISTHMUS) {
            l1_block_info.operator_fee_charge(
                enveloped_tx,
                U256::from(frame_result.gas().used()),
                spec,
            )
        } else {
            U256::ZERO
        };
        let base_fee_amount = U256::from(basefee.saturating_mul(frame_result.gas().used() as u128));

        // Send fees to their respective recipients
        for (recipient, amount) in [
            (L1_FEE_RECIPIENT, l1_cost),
            (BASE_FEE_RECIPIENT, base_fee_amount),
            (OPERATOR_FEE_RECIPIENT, operator_fee_cost),
        ] {
            ctx.journal_mut().balance_incr(recipient, amount)?;
        }

        Ok(())
    }

    fn execution_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        match core::mem::replace(evm.ctx().error(), Ok(())) {
            Err(ContextError::Db(e)) => return Err(e.into()),
            Err(ContextError::Custom(e)) => return Err(Self::Error::from_string(e)),
            Ok(_) => (),
        }

        let exec_result =
            post_execution::output(evm.ctx(), frame_result).map_haltreason(OpHaltReason::Base);

        if exec_result.is_halt() {
            let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
            if is_deposit && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH) {
                return Err(ERROR::from(OpTransactionError::HaltedDepositPostRegolith));
            }
        }
        evm.ctx().journal_mut().commit_tx();
        evm.ctx().chain_mut().clear_tx_l1_cost();
        evm.ctx().local_mut().clear();
        evm.frame_stack().clear();

        Ok(exec_result)
    }

    fn catch_error(
        &self,
        evm: &mut Self::Evm,
        error: Self::Error,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_tx_error = error.is_tx_error();
        let mut output = Err(error);

        // Deposit transaction can't fail so we manually handle it here.
        if is_tx_error && is_deposit {
            let ctx = evm.ctx();
            let spec = ctx.cfg().spec();
            let tx = ctx.tx();
            let caller = tx.caller();
            let mint = tx.mint();
            let is_system_tx = tx.is_system_transaction();
            let gas_limit = tx.gas_limit();
            let journal = evm.ctx().journal_mut();

            // discard all changes of this transaction
            // Default JournalCheckpoint is the first checkpoint and will wipe all changes.
            journal.checkpoint_revert(JournalCheckpoint::default());

            let mut acc = journal.load_account_mut(caller)?;
            acc.bump_nonce();
            acc.incr_balance(U256::from(mint.unwrap_or_default()));

            drop(acc); // Drop acc to avoid borrow checker issues.

            // We can now commit the changes.
            journal.commit_tx();

            let gas_used =
                if spec.is_enabled_in(OpSpecId::REGOLITH) || !is_system_tx { gas_limit } else { 0 };
            // clear the journal
            output = Ok(ExecutionResult::Halt { reason: OpHaltReason::FailedDeposit, gas_used })
        }

        // do the cleanup
        evm.ctx().chain_mut().clear_tx_l1_cost();
        evm.ctx().local_mut().clear();
        evm.frame_stack().clear();

        output
    }
}

impl<EVM, ERROR> InspectorHandler for OpHandler<EVM, ERROR, EthFrame<EthInterpreter>>
where
    EVM: InspectorEvmTr<
            Context: OpContextTr,
            Frame = EthFrame<EthInterpreter>,
            Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
        >,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
{
    type IT = EthInterpreter;
}

#[cfg(test)]
mod tests {

    use alloy_primitives::uint;
    use revm::{
        bytecode::Bytecode,
        context::{BlockEnv, CfgEnv, Context, TxEnv},
        database::InMemoryDB,
        database_interface::EmptyDB,
        handler::{EthFrame, Handler},
        interpreter::{CallOutcome, InstructionResult, InterpreterResult},
        primitives::{Address, B256, Bytes, TxKind, bytes, hardfork::SpecId},
        state::AccountInfo,
    };

    use super::*;
    use crate::{
        DefaultOp, OpBuilder, OpContext, OpTransaction,
        constants::{
            BASE_FEE_SCALAR_OFFSET, ECOTONE_L1_BLOB_BASE_FEE_SLOT, ECOTONE_L1_FEE_SCALARS_SLOT,
            L1_BASE_FEE_SLOT, L1_BLOCK_CONTRACT, OPERATOR_FEE_SCALARS_SLOT,
        },
    };

    /// Creates frame result.
    fn call_last_frame_return(
        ctx: OpContext<EmptyDB>,
        instruction_result: InstructionResult,
        gas: Gas,
    ) -> Gas {
        let mut evm = ctx.build_op();

        let mut exec_result = FrameResult::Call(CallOutcome::new(
            InterpreterResult { result: instruction_result, output: Bytes::new(), gas },
            0..0,
        ));

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        handler.last_frame_result(&mut evm, &mut exec_result).unwrap();
        handler.refund(&mut evm, &mut exec_result, 0);
        *exec_result.gas()
    }

    #[test]
    fn test_revert_gas() {
        let ctx = Context::op()
            .with_tx(OpTransaction::builder().base(TxEnv::builder().gas_limit(100)).build_fill())
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK));

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas() {
        let ctx = Context::op()
            .with_tx(OpTransaction::builder().base(TxEnv::builder().gas_limit(100)).build_fill())
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH));

        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_with_refund() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::from([1u8; 32]))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH));

        let mut ret_gas = Gas::new(90);
        ret_gas.record_refund(20);

        let gas = call_last_frame_return(ctx.clone(), InstructionResult::Stop, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 2); // min(20, 10/5)

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_deposit_tx() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::from([1u8; 32]))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK));
        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 0);
        assert_eq!(gas.spent(), 100);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_sys_deposit_tx() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::from([1u8; 32]))
                    .is_system_transaction()
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK));
        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 100);
        assert_eq!(gas.spent(), 0);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_commit_mint_value() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo { balance: U256::from(1000), ..Default::default() },
        );

        let mut ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH));
        ctx.modify_tx(|tx| {
            tx.deposit.source_hash = B256::from([1u8; 32]);
            tx.deposit.mint = Some(10);
        });

        let mut evm = ctx.build_op();

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler.validate_against_state_and_deduct_caller(&mut evm).unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1010));
    }

    #[test]
    fn test_remove_l1_cost_non_deposit() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1058), // Increased to cover L1 fees (1048) + base fees
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                l2_block: Some(U256::from(0)),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::REGOLITH))
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .source_hash(B256::ZERO)
                    .build()
                    .unwrap(),
            );

        let mut evm = ctx.build_op();

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler.validate_against_state_and_deduct_caller(&mut evm).unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(10)); // 1058 - 1048 = 10
    }

    #[test]
    fn test_reload_l1_block_info_isthmus() {
        const BLOCK_NUM: U256 = uint!(100_U256);
        const L1_BASE_FEE: U256 = uint!(1_U256);
        const L1_BLOB_BASE_FEE: U256 = uint!(2_U256);
        const L1_BASE_FEE_SCALAR: u64 = 3;
        const L1_BLOB_BASE_FEE_SCALAR: u64 = 4;
        const L1_FEE_SCALARS: U256 = U256::from_limbs([
            0,
            (L1_BASE_FEE_SCALAR << (64 - BASE_FEE_SCALAR_OFFSET * 2)) | L1_BLOB_BASE_FEE_SCALAR,
            0,
            0,
        ]);
        const OPERATOR_FEE_SCALAR: u64 = 5;
        const OPERATOR_FEE_CONST: u64 = 6;
        const OPERATOR_FEE: U256 =
            U256::from_limbs([OPERATOR_FEE_CONST, OPERATOR_FEE_SCALAR, 0, 0]);

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract.storage.insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        l1_block_contract.storage.insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, L1_BLOB_BASE_FEE);
        l1_block_contract.storage.insert(ECOTONE_L1_FEE_SCALARS_SLOT, L1_FEE_SCALARS);
        l1_block_contract.storage.insert(OPERATOR_FEE_SCALARS_SLOT, OPERATOR_FEE);
        db.insert_account_info(
            Address::ZERO,
            AccountInfo { balance: U256::from(1000), ..Default::default() },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM + U256::from(1)), // ahead by one block
                ..Default::default()
            })
            .with_block(BlockEnv { number: BLOCK_NUM, ..Default::default() })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::ISTHMUS));

        let mut evm = ctx.build_op();

        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler.validate_against_state_and_deduct_caller(&mut evm).unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                l1_blob_base_fee: Some(L1_BLOB_BASE_FEE),
                l1_blob_base_fee_scalar: Some(U256::from(L1_BLOB_BASE_FEE_SCALAR)),
                empty_ecotone_scalars: false,
                l1_fee_overhead: None,
                operator_fee_scalar: Some(U256::from(OPERATOR_FEE_SCALAR)),
                operator_fee_constant: Some(U256::from(OPERATOR_FEE_CONST)),
                tx_l1_cost: Some(U256::ZERO),
                da_footprint_gas_scalar: None
            }
        );
    }

    #[test]
    fn test_base_v1_tx_gas_limit_cap_rejected() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(16_777_217))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.validate_env(&mut evm);
        assert!(result.is_err(), "gas_limit above cap should be rejected");
    }

    #[test]
    fn test_base_v1_tx_gas_limit_at_cap_ok() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(16_777_216))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.validate_env(&mut evm);
        assert!(result.is_ok(), "gas_limit at cap should be accepted");
    }

    #[test]
    fn test_jovian_no_tx_gas_limit_cap() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(16_777_217))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::JOVIAN));
        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.validate_env(&mut evm);
        assert!(result.is_ok(), "Jovian should not enforce gas limit cap");
    }

    #[test]
    fn test_base_v1_deposit_skips_gas_limit_cap() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(16_777_217))
                    .source_hash(B256::from([1u8; 32]))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.validate_env(&mut evm);
        assert!(result.is_ok(), "deposit txs should skip gas limit cap");
    }

    #[test]
    fn test_osaka_opcodes_activated_base_v1() {
        assert_eq!(OpSpecId::BASE_V1.into_eth_spec(), SpecId::OSAKA);
    }

    /// Runs CLZ bytecode (`PUSH1 0x80, CLZ, PUSH1 0x00, MSTORE, PUSH1 0x20, PUSH1 0x00, RETURN`)
    /// against the given spec and returns the execution result.
    fn run_clz_bytecode(
        spec: OpSpecId,
    ) -> revm::context_interface::result::ExecutionResult<OpHaltReason> {
        let contract = Address::from([0x42; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            contract,
            AccountInfo {
                code: Some(Bytecode::new_legacy(bytes!("60801E60005260206000F3"))),
                ..Default::default()
            },
        );
        db.insert_account_info(
            Address::ZERO,
            AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100_000).kind(TxKind::Call(contract)))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            )
            .with_cfg(CfgEnv::new_with_spec(spec))
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            });
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler.run(&mut evm).unwrap()
    }

    #[test]
    fn test_clz_opcode_base_v1() {
        let result = run_clz_bytecode(OpSpecId::BASE_V1);
        assert!(result.is_success(), "CLZ opcode should execute successfully on BASE_V1");

        let output = result.output().unwrap();
        let expected = U256::from(248);
        let actual = U256::from_be_slice(output);
        assert_eq!(actual, expected, "CLZ of 0x80 in 256-bit should be 248");
    }

    #[test]
    fn test_clz_opcode_not_on_jovian() {
        let result = run_clz_bytecode(OpSpecId::JOVIAN);
        assert!(!result.is_success(), "CLZ opcode should not be available on JOVIAN (pre-OSAKA)");
    }

    // -----------------------------------------------------------------------
    // EIP-8130 handler execution tests
    // -----------------------------------------------------------------------

    use crate::{
        Eip8130Call, Eip8130CodePlacement, Eip8130Parts, Eip8130VerifyCall,
        decode_phase_statuses,
    };

    /// Builds an EVM with EIP-8130 parts and runs the full handler flow,
    /// returning the execution result.
    fn run_eip8130_tx(
        sender: Address,
        accounts: &[(Address, Bytecode)],
        eip8130: Eip8130Parts,
        gas_limit: u64,
    ) -> ExecutionResult<OpHaltReason> {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            NONCE_MANAGER_ADDRESS,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("FE"))), ..Default::default() },
        );
        for (addr, code) in accounts {
            db.insert_account_info(
                *addr,
                AccountInfo { code: Some(code.clone()), ..Default::default() },
            );
        }

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(gas_limit)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = eip8130;

        let ctx = Context::op()
            .with_db(db)
            .with_tx(tx)
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler.run(&mut evm).unwrap()
    }

    #[test]
    fn test_eip8130_empty_phases_reverts() {
        let sender = Address::from([0x11; 20]);
        let result = run_eip8130_tx(
            sender,
            &[],
            Eip8130Parts { sender, payer: sender, ..Default::default() },
            100_000,
        );
        assert!(!result.is_success(), "empty phases = no successes = tx reverts");
    }

    #[test]
    fn test_eip8130_deploy_only_succeeds() {
        let sender = Address::from([0x11; 20]);
        let deployed_addr = Address::from([0x99; 20]);
        let bytecode = bytes!("363d3d373d3d3d363d73DEADBEEF5af43d82803e903d91602b57fd5bf3");

        let result = run_eip8130_tx(
            sender,
            &[],
            Eip8130Parts {
                sender,
                payer: sender,
                has_create_entry: true,
                code_placements: vec![Eip8130CodePlacement {
                    address: deployed_addr,
                    code: bytecode,
                }],
                ..Default::default()
            },
            100_000,
        );
        assert!(result.is_success(), "deploy-only tx should succeed");

        let statuses = decode_phase_statuses(result.output().unwrap());
        assert!(statuses.is_empty(), "no call phases = empty statuses");
    }

    #[test]
    fn test_eip8130_single_phase_success() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);

        let result = run_eip8130_tx(
            sender,
            &[(target, Bytecode::new_legacy(bytes!("00")))], // STOP
            Eip8130Parts {
                sender,
                payer: sender,
                call_phases: vec![vec![Eip8130Call {
                    to: target,
                    data: Bytes::new(),
                    value: U256::ZERO,
                }]],
                ..Default::default()
            },
            100_000,
        );
        assert!(result.is_success(), "single STOP call should succeed");

        let statuses = decode_phase_statuses(result.output().unwrap());
        assert_eq!(statuses, vec![true]);
    }

    #[test]
    fn test_eip8130_single_phase_failure_reverts_tx() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);

        let result = run_eip8130_tx(
            sender,
            &[(target, Bytecode::new_legacy(bytes!("60006000FD")))], // REVERT
            Eip8130Parts {
                sender,
                payer: sender,
                call_phases: vec![vec![Eip8130Call {
                    to: target,
                    data: Bytes::new(),
                    value: U256::ZERO,
                }]],
                ..Default::default()
            },
            100_000,
        );
        assert!(!result.is_success(), "all phases failed → tx reverts");
    }

    #[test]
    fn test_eip8130_mixed_phases_succeeds() {
        let sender = Address::from([0x11; 20]);
        let target_ok = Address::from([0x22; 20]);
        let target_fail = Address::from([0x33; 20]);

        let result = run_eip8130_tx(
            sender,
            &[
                (target_ok, Bytecode::new_legacy(bytes!("00"))), // STOP
                (target_fail, Bytecode::new_legacy(bytes!("60006000FD"))), // REVERT
            ],
            Eip8130Parts {
                sender,
                payer: sender,
                call_phases: vec![
                    vec![Eip8130Call { to: target_ok, data: Bytes::new(), value: U256::ZERO }],
                    vec![Eip8130Call { to: target_fail, data: Bytes::new(), value: U256::ZERO }],
                ],
                ..Default::default()
            },
            100_000,
        );
        assert!(result.is_success(), "at least one phase succeeded → tx succeeds");

        let statuses = decode_phase_statuses(result.output().unwrap());
        assert_eq!(statuses, vec![true, false]);
    }

    #[test]
    fn test_eip8130_all_phases_fail_reverts_tx() {
        let sender = Address::from([0x11; 20]);
        let target_fail = Address::from([0x33; 20]);

        let result = run_eip8130_tx(
            sender,
            &[(target_fail, Bytecode::new_legacy(bytes!("60006000FD")))], // REVERT
            Eip8130Parts {
                sender,
                payer: sender,
                call_phases: vec![
                    vec![Eip8130Call { to: target_fail, data: Bytes::new(), value: U256::ZERO }],
                    vec![Eip8130Call { to: target_fail, data: Bytes::new(), value: U256::ZERO }],
                ],
                ..Default::default()
            },
            100_000,
        );
        assert!(!result.is_success(), "all phases failed → tx reverts");
    }

    #[test]
    fn test_eip8130_gas_accounting() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);

        let aa_intrinsic = 25_000u64;
        let gas_limit = 100_000u64;
        let result = run_eip8130_tx(
            sender,
            &[(target, Bytecode::new_legacy(bytes!("00")))], // STOP
            Eip8130Parts {
                sender,
                payer: sender,
                aa_intrinsic_gas: aa_intrinsic,
                call_phases: vec![vec![Eip8130Call {
                    to: target,
                    data: Bytes::new(),
                    value: U256::ZERO,
                }]],
                ..Default::default()
            },
            gas_limit,
        );
        assert!(result.is_success());
        assert!(result.gas_used() >= aa_intrinsic, "at least intrinsic gas should be charged");
        assert!(result.gas_used() <= gas_limit, "cannot spend more than limit");
    }

    #[test]
    fn test_eip8130_owner_id_visible_through_tx_context() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x44; 20]);
        let owner_id = B256::from([0xAB; 32]);

        // Runtime for OwnerIdProbe:
        // - probe(): reads TxContext.getOwnerId() and stores it at slot 0
        // - lastOwnerId(): returns slot 0
        let probe_runtime = Bytecode::new_legacy(bytes!(
            "608060405234801561000f575f5ffd5b5060043610610034575f3560e01c80634320a6cb14610038578063b74af5a914610056575b5f5ffd5b610040610074565b60405161004d9190610111565b60405180910390f35b61005e610079565b60405161006b9190610111565b60405180910390f35b5f5481565b5f5f61aa0373ffffffffffffffffffffffffffffffffffffffff16631f5072f26040518163ffffffff1660e01b8152600401602060405180830381865afa1580156100c6573d5f5f3e3d5ffd5b505050506040513d601f19601f820116820180604052508101906100ea9190610158565b9050805f819055508091505090565b5f819050919050565b61010b816100f9565b82525050565b5f6020820190506101245f830184610102565b92915050565b5f5ffd5b610137816100f9565b8114610141575f5ffd5b50565b5f815190506101528161012e565b92915050565b5f6020828403121561016d5761016c61012a565b5b5f61017a84828501610144565b9150509291505056fea26469706673582212203ca48096bb84d6eb04b36713b485cfdc832bcb25ec90dc9b384decb8a8ba23ee64736f6c63430008210033"
        ));

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            NONCE_MANAGER_ADDRESS,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("FE"))), ..Default::default() },
        );
        db.insert_account_info(
            target,
            AccountInfo { code: Some(probe_runtime), ..Default::default() },
        );

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(300_000)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = Eip8130Parts {
            sender,
            payer: sender,
            owner_id,
            call_phases: vec![vec![Eip8130Call {
                to: target,
                data: bytes!("b74af5a9"), // probe()
                value: U256::ZERO,
            }]],
            ..Default::default()
        };

        let ctx = Context::op()
            .with_db(db)
            .with_tx(tx)
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.run(&mut evm).unwrap();
        assert!(result.is_success(), "probe call should succeed");

        let account =
            evm.ctx().journal_mut().load_account(target).expect("target account should be loaded");
        let slot = account.storage.get(&U256::ZERO).expect("probe should write slot 0");
        assert_eq!(
            slot.present_value(),
            U256::from_be_bytes(owner_id.0),
            "slot0 should store owner_id"
        );
    }

    // -----------------------------------------------------------------------
    // Custom verifier STATICCALL tests
    // -----------------------------------------------------------------------

    /// Builds bytecode that returns a fixed 32-byte value (owner_id).
    ///
    /// Bytecode: PUSH32 <id> | PUSH1 0 | MSTORE | PUSH1 32 | PUSH1 0 | RETURN
    fn make_verifier_bytecode(owner_id: B256) -> Bytecode {
        let mut code = vec![0x7f]; // PUSH32
        code.extend_from_slice(owner_id.as_slice());
        code.extend_from_slice(&[
            0x60, 0x00, // PUSH1 0
            0x52, // MSTORE
            0x60, 0x20, // PUSH1 32
            0x60, 0x00, // PUSH1 0
            0xF3, // RETURN
        ]);
        Bytecode::new_legacy(Bytes::from(code))
    }

    /// Packs `(verifier_address, scope)` into the 32-byte word format used by
    /// AccountConfig's owner_config mapping.
    fn pack_owner_config(verifier: Address, scope: u8) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[0] = scope;
        bytes[12..32].copy_from_slice(verifier.as_slice());
        U256::from_be_bytes(bytes)
    }

    #[test]
    fn test_eip8130_custom_verifier_staticcall_succeeds() {
        let sender = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);
        let verifier = Address::from([0xAA; 20]);
        let owner_id = B256::from([0xBB; 32]);

        let owner_config_slot =
            aa_owner_config_slot(sender, U256::from_be_bytes(owner_id.0));
        let packed_config = pack_owner_config(verifier, 0x00); // scope=0 = unrestricted

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            NONCE_MANAGER_ADDRESS,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("FE"))), ..Default::default() },
        );
        db.insert_account_info(
            verifier,
            AccountInfo {
                code: Some(make_verifier_bytecode(owner_id)),
                ..Default::default()
            },
        );
        db.insert_account_info(
            target,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("00"))), ..Default::default() },
        );
        db.insert_account_info(
            ACCOUNT_CONFIG_ADDRESS,
            AccountInfo::default(),
        );
        let acfg = db.load_account(ACCOUNT_CONFIG_ADDRESS).unwrap();
        acfg.storage.insert(owner_config_slot, packed_config);

        let calldata = Bytes::from(vec![0xCA; 36]); // dummy calldata

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(200_000)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = Eip8130Parts {
            sender,
            payer: sender,
            call_phases: vec![vec![Eip8130Call {
                to: target,
                data: Bytes::new(),
                value: U256::ZERO,
            }]],
            sender_verify_call: Some(Eip8130VerifyCall {
                verifier,
                calldata,
                account: sender,
                required_scope: 0x02, // SENDER
            }),
            ..Default::default()
        };

        let ctx = Context::op()
            .with_db(db)
            .with_tx(tx)
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.run(&mut evm).unwrap();
        assert!(result.is_success(), "custom verifier STATICCALL should succeed");

        let statuses = decode_phase_statuses(result.output().unwrap());
        assert_eq!(statuses, vec![true]);
    }

    #[test]
    fn test_eip8130_custom_verifier_wrong_verifier_fails() {
        let sender = Address::from([0x11; 20]);
        let verifier = Address::from([0xAA; 20]);
        let wrong_verifier = Address::from([0xCC; 20]); // different from expected
        let owner_id = B256::from([0xBB; 32]);

        let owner_config_slot =
            aa_owner_config_slot(sender, U256::from_be_bytes(owner_id.0));
        // Store a DIFFERENT verifier in owner_config than what the tx specifies
        let packed_config = pack_owner_config(wrong_verifier, 0x00);

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            NONCE_MANAGER_ADDRESS,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("FE"))), ..Default::default() },
        );
        db.insert_account_info(
            verifier,
            AccountInfo {
                code: Some(make_verifier_bytecode(owner_id)),
                ..Default::default()
            },
        );
        db.insert_account_info(
            ACCOUNT_CONFIG_ADDRESS,
            AccountInfo::default(),
        );
        let acfg = db.load_account(ACCOUNT_CONFIG_ADDRESS).unwrap();
        acfg.storage.insert(owner_config_slot, packed_config);

        let calldata = Bytes::from(vec![0xCA; 36]);

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(200_000)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = Eip8130Parts {
            sender,
            payer: sender,
            call_phases: vec![],
            sender_verify_call: Some(Eip8130VerifyCall {
                verifier,
                calldata,
                account: sender,
                required_scope: 0x02,
            }),
            ..Default::default()
        };

        let ctx = Context::op()
            .with_db(db)
            .with_tx(tx)
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.run(&mut evm);
        assert!(result.is_err(), "mismatched verifier should cause an error");
    }

    #[test]
    fn test_eip8130_custom_verifier_wrong_scope_fails() {
        let sender = Address::from([0x11; 20]);
        let verifier = Address::from([0xAA; 20]);
        let owner_id = B256::from([0xBB; 32]);

        let owner_config_slot =
            aa_owner_config_slot(sender, U256::from_be_bytes(owner_id.0));
        // Scope = PAYER (0x04), but required is SENDER (0x02) → should fail
        let packed_config = pack_owner_config(verifier, 0x04);

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            sender,
            AccountInfo { balance: U256::from(10_000_000), ..Default::default() },
        );
        db.insert_account_info(
            NONCE_MANAGER_ADDRESS,
            AccountInfo { code: Some(Bytecode::new_legacy(bytes!("FE"))), ..Default::default() },
        );
        db.insert_account_info(
            verifier,
            AccountInfo {
                code: Some(make_verifier_bytecode(owner_id)),
                ..Default::default()
            },
        );
        db.insert_account_info(
            ACCOUNT_CONFIG_ADDRESS,
            AccountInfo::default(),
        );
        let acfg = db.load_account(ACCOUNT_CONFIG_ADDRESS).unwrap();
        acfg.storage.insert(owner_config_slot, packed_config);

        let calldata = Bytes::from(vec![0xCA; 36]);

        let mut tx = OpTransaction::builder()
            .base(
                TxEnv::builder()
                    .tx_type(Some(0x05))
                    .caller(sender)
                    .gas_limit(200_000)
                    .kind(TxKind::Call(sender)),
            )
            .enveloped_tx(Some(bytes!("05FACADE")))
            .build_fill();
        tx.eip8130 = Eip8130Parts {
            sender,
            payer: sender,
            call_phases: vec![],
            sender_verify_call: Some(Eip8130VerifyCall {
                verifier,
                calldata,
                account: sender,
                required_scope: 0x02, // SENDER
            }),
            ..Default::default()
        };

        let ctx = Context::op()
            .with_db(db)
            .with_tx(tx)
            .with_chain(L1BlockInfo {
                l2_block: Some(U256::ZERO),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BASE_V1));
        let mut evm = ctx.build_op();

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        let result = handler.run(&mut evm);
        assert!(result.is_err(), "wrong scope should cause an error");
    }
}
