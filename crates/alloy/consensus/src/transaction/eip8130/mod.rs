//! EIP-8130: Account Abstraction by Account Configuration.
//!
//! Defines the AA transaction type (`TxEip8130`), supporting types (calls, owners,
//! account change entries), constants, signature hash computation, intrinsic gas
//! calculation, and CREATE2 address derivation.

mod constants;
pub use constants::{
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
    CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, EOA_AUTH_GAS, DEPLOYMENT_HEADER_SIZE,
    MAX_SIGNATURE_SIZE, NONCE_KEY_COLD_GAS, NONCE_KEY_WARM_GAS, SLOAD_GAS, VERIFIER_CUSTOM,
    VERIFIER_DELEGATE, VERIFIER_K1, VERIFIER_P256_RAW, VERIFIER_P256_WEBAUTHN,
};

mod types;
pub use types::{
    AccountChangeEntry, Call, ConfigChangeEntry, ConfigOperation, CreateEntry, Owner, OwnerScope,
    CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, OP_AUTHORIZE_OWNER, OP_REVOKE_OWNER,
};

mod tx;
pub use tx::TxEip8130;

mod signature;
pub use signature::{
    ParsedSenderAuth, VerifierTarget, payer_signature_hash, parse_sender_auth, resolve_verifier,
    sender_signature_hash,
};

mod gas;
pub use gas::{
    account_changes_cost, bytecode_cost, intrinsic_gas, nonce_key_cost, payer_auth_cost,
    sender_auth_cost, tx_payload_cost,
};

mod address;
pub use address::{
    create2_address, deployment_code, deployment_header, derive_account_address, effective_salt,
};

mod abi;
pub use abi::{
    CallTuple, ConfigOpTuple, IAccountConfig, INonceManager, ITxContext, IVerifier, OwnerTuple,
};

mod predeploys;
pub use predeploys::{
    ACCOUNT_CONFIG_ADDRESS, DEFAULT_ACCOUNT_ADDRESS, DELEGATE_VERIFIER_ADDRESS,
    K1_VERIFIER_ADDRESS, NONCE_MANAGER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, TX_CONTEXT_ADDRESS,
};

mod storage;
pub use storage::{
    LOCK_BASE_SLOT, NONCE_BASE_SLOT, OWNER_CONFIG_BASE_SLOT, SEQUENCE_BASE_SLOT,
    encode_owner_config, lock_slot, nonce_slot, owner_config_slot, parse_owner_config,
    read_sequence, sequence_base_slot, write_sequence,
};
#[allow(deprecated)]
pub use storage::sequence_slot;

#[cfg(feature = "evm")]
mod accessors;
#[cfg(feature = "evm")]
pub use accessors::{
    LockState, increment_nonce_op, is_owner_authorized, read_change_sequence, read_lock_state,
    read_nonce, read_owner_config, write_owner_config_op,
};

#[cfg(feature = "evm")]
mod execution;
#[cfg(feature = "evm")]
pub use execution::{
    Eip8130ExecutionPlan, BalanceTransfer, CodePlacement, ExecutionCall, PhaseResult,
    SequenceUpdateInfo, StorageWrite, TxContextValues, auto_delegation_code,
    build_execution_calls, config_change_sequence, config_change_writes, gas_refund,
    max_gas_cost, nonce_increment_write, owner_registration_writes,
};

#[cfg(feature = "evm")]
mod precompiles;
#[cfg(feature = "evm")]
pub use precompiles::{
    NONCE_MANAGER_GAS, PrecompileError, TX_CONTEXT_GAS, handle_nonce_manager, handle_tx_context,
};

#[cfg(feature = "evm")]
mod validation;
#[cfg(feature = "evm")]
pub use validation::{
    ValidationError, ValidationResult, check_lock_state, check_payer_authorization,
    check_sender_authorization, decode_verify_return, encode_verify_call, implicit_eoa_owner_id,
    resolve_sender, validate_config_change_sequences, validate_expiry, validate_nonce,
    validate_structure, verifier_type_to_address,
};

#[cfg(feature = "native-verifier")]
mod native_verifier;
#[cfg(feature = "native-verifier")]
pub use native_verifier::{NativeVerifyError, NativeVerifyResult, try_native_verify};

#[cfg(test)]
mod tests;
