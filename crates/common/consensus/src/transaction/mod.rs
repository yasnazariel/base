//! Transaction types for Base chains.

mod deposit;
pub use deposit::{DepositTransaction, TxDeposit};

mod eip8130;
pub use eip8130::{
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, ACCOUNT_CONFIG_ADDRESS, AccountChangeEntry,
    BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS, CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS,
    Call, CallTuple, ConfigChangeEntry, ConfigOpTuple, ConfigOperation, CreateEntry,
    DEFAULT_ACCOUNT_ADDRESS,
    DELEGATE_VERIFIER_ADDRESS, DEPLOYMENT_HEADER_SIZE, EOA_AUTH_GAS, IAccountConfig,
    INonceManager, ITxContext, IVerifier, K1_VERIFIER_ADDRESS, LOCK_BASE_SLOT,
    MAX_SIGNATURE_SIZE, NONCE_BASE_SLOT, NONCE_KEY_COLD_GAS, NONCE_KEY_WARM_GAS,
    NONCE_MANAGER_ADDRESS, OWNER_CONFIG_BASE_SLOT, Owner, OwnerScope, OwnerTuple,
    P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, ParsedSenderAuth, SEQUENCE_BASE_SLOT, SLOAD_GAS,
    TX_CONTEXT_ADDRESS, TxAa, VERIFIER_CUSTOM, VERIFIER_DELEGATE, VERIFIER_K1,
    VERIFIER_P256_RAW, VERIFIER_P256_WEBAUTHN, VerifierTarget, account_changes_cost,
    bytecode_cost, create2_address, deployment_code, deployment_header, derive_account_address,
    effective_salt, encode_owner_config, intrinsic_gas, lock_slot, nonce_key_cost, nonce_slot,
    owner_config_slot, parse_owner_config, payer_auth_cost, payer_signature_hash,
    parse_sender_auth, resolve_verifier, sender_auth_cost, sender_signature_hash,
    sequence_slot, tx_payload_cost, CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE,
    OP_AUTHORIZE_OWNER, OP_REVOKE_OWNER,
};
#[cfg(feature = "evm")]
pub use eip8130::{
    AaExecutionPlan, BalanceTransfer, CodePlacement, ExecutionCall, LockState, NONCE_MANAGER_GAS,
    PhaseResult, PrecompileError, TX_CONTEXT_GAS,
    StorageWrite, TxContextValues, ValidationError, ValidationResult, auto_delegation_code,
    build_execution_calls, check_lock_state, check_payer_authorization,
    check_sender_authorization, config_change_writes, decode_verify_return, encode_verify_call,
    gas_refund, handle_nonce_manager, handle_tx_context, implicit_eoa_owner_id,
    increment_nonce_op, is_owner_authorized, max_gas_cost,
    nonce_increment_write, owner_registration_writes, read_change_sequence, read_lock_state,
    read_nonce, read_owner_config, resolve_sender, validate_config_change_sequences,
    validate_expiry, validate_nonce, validate_structure, verifier_type_to_address,
    write_owner_config_op,
};
#[cfg(feature = "native-verifier")]
pub use eip8130::{NativeVerifyError, NativeVerifyResult, try_native_verify};

mod tx_type;
pub use tx_type::DEPOSIT_TX_TYPE_ID;

mod envelope;
pub use envelope::{OpAaTransaction, OpTransaction, OpTxEnvelope, OpTxType};

mod typed;
pub use typed::OpTypedTransaction;

mod pooled;
#[cfg(feature = "serde")]
pub use deposit::serde_deposit_tx_rpc;
pub use pooled::OpPooledTransaction;

mod meta;
pub use meta::{OpDepositInfo, OpTransactionInfo};

/// Bincode-compatible serde implementations for transaction types.
#[cfg(all(feature = "serde", feature = "serde-bincode-compat"))]
pub mod serde_bincode_compat {
    pub use super::{deposit::serde_bincode_compat::TxDeposit, envelope::serde_bincode_compat::*};
}
