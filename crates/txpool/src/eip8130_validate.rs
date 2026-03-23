//! Proper mempool validation for EIP-8130 (AA) transactions.
//!
//! Validates nonce, expiry, sender/payer authorization (with native Rust
//! crypto for K1 and P256 verifiers), and payer balance before accepting
//! an AA transaction into the pending pool.

use alloy_consensus::Transaction;
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Address, B256, Bytes, U256};
use reth_storage_api::StateProviderFactory;

use base_alloy_consensus::{
    ACCOUNT_CONFIG_ADDRESS, K1_VERIFIER_ADDRESS, NONCE_MANAGER_ADDRESS, NativeVerifyResult,
    OpPooledTransaction, OwnerScope, ParsedSenderAuth, TxEip8130, ValidationError, VerifierTarget,
    VERIFIER_K1, implicit_eoa_owner_id, nonce_slot, owner_config_slot, parse_owner_config,
    parse_sender_auth, payer_signature_hash, resolve_verifier, sender_signature_hash,
    try_native_verify, validate_expiry, validate_structure, verifier_type_to_address,
};

use crate::OpPooledTx;

/// Successful AA validation outcome, providing the data the txpool needs for
/// ordering and balance tracking.
#[derive(Debug)]
pub struct Eip8130ValidationOutcome {
    /// Payer's balance (used for txpool cost checks).
    pub balance: U256,
    /// The sender's current nonce_sequence (used for txpool nonce ordering).
    pub state_nonce: u64,
}

/// Errors from AA transaction validation.
#[derive(Debug)]
pub enum Eip8130ValidationError {
    /// Failed to decode the `TxEip8130` from 2718-encoded bytes.
    DecodeFailed(String),
    /// Structural validation failed (sizes, nonce_key range, account_changes).
    Structural(ValidationError),
    /// Transaction has expired.
    Expired {
        /// Transaction's expiry timestamp.
        expiry: u64,
        /// Current block timestamp.
        current: u64,
    },
    /// Nonce does not match the on-chain value.
    NonceMismatch {
        /// On-chain nonce.
        expected: u64,
        /// Transaction's nonce_sequence.
        got: u64,
    },
    /// `sender_auth` is malformed or signature verification failed.
    SenderAuthInvalid(String),
    /// Sender's owner is not authorized in AccountConfig.
    SenderNotAuthorized(String),
    /// `payer_auth` is malformed or signature verification failed.
    PayerAuthInvalid(String),
    /// Payer's owner is not authorized in AccountConfig.
    PayerNotAuthorized(String),
    /// Payer has insufficient balance to cover `gas_limit * max_fee_per_gas`.
    InsufficientBalance {
        /// Required balance.
        required: U256,
        /// Available balance.
        available: U256,
    },
    /// Error reading on-chain state.
    StateError(String),
}

impl std::fmt::Display for Eip8130ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DecodeFailed(e) => write!(f, "decode failed: {e}"),
            Self::Structural(e) => write!(f, "structural: {e}"),
            Self::Expired { expiry, current } => {
                write!(f, "expired (expiry={expiry}, current={current})")
            }
            Self::NonceMismatch { expected, got } => {
                write!(f, "nonce mismatch (expected={expected}, got={got})")
            }
            Self::SenderAuthInvalid(e) => write!(f, "sender auth invalid: {e}"),
            Self::SenderNotAuthorized(e) => write!(f, "sender not authorized: {e}"),
            Self::PayerAuthInvalid(e) => write!(f, "payer auth invalid: {e}"),
            Self::PayerNotAuthorized(e) => write!(f, "payer not authorized: {e}"),
            Self::InsufficientBalance { required, available } => {
                write!(f, "payer insufficient balance (required={required}, available={available})")
            }
            Self::StateError(e) => write!(f, "state access error: {e}"),
        }
    }
}

impl std::error::Error for Eip8130ValidationError {}

impl reth_transaction_pool::error::PoolTransactionError for Eip8130ValidationError {
    fn is_bad_transaction(&self) -> bool {
        matches!(self, Self::Structural(_) | Self::DecodeFailed(_))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Decodes `TxEip8130` from a 2718-encoded pool transaction.
fn decode_tx_eip8130<Tx: OpPooledTx>(transaction: &Tx) -> Result<TxEip8130, Eip8130ValidationError> {
    let encoded = transaction.encoded_2718();
    let bytes: &[u8] = encoded.as_ref();
    let pooled = OpPooledTransaction::decode_2718(&mut &bytes[..])
        .map_err(|e| Eip8130ValidationError::DecodeFailed(e.to_string()))?;
    match pooled {
        OpPooledTransaction::Eip8130(sealed) => Ok(sealed.into_inner()),
        _ => Err(Eip8130ValidationError::DecodeFailed("not an AA transaction".into())),
    }
}

/// Reads a storage slot from a state provider, returning U256::ZERO if absent.
fn read_storage(
    state: &dyn reth_storage_api::StateProvider,
    address: Address,
    slot: B256,
) -> Result<U256, Eip8130ValidationError> {
    state
        .storage(address, slot.into())
        .map(|v| v.unwrap_or_default())
        .map_err(|e| Eip8130ValidationError::StateError(e.to_string()))
}

/// Reads `owner_config(account, owner_id)` from AccountConfig storage.
///
/// Returns `(verifier_address, scope)`.
fn read_owner_config_from_state(
    state: &dyn reth_storage_api::StateProvider,
    account: Address,
    owner_id: B256,
) -> Result<(Address, u8), Eip8130ValidationError> {
    let slot = owner_config_slot(account, owner_id);
    let value = read_storage(state, ACCOUNT_CONFIG_ADDRESS, slot)?;
    Ok(parse_owner_config(B256::from(value.to_be_bytes::<32>())))
}

/// Validates `sender_auth` for an AA transaction.
///
/// For EOA mode (from == 0): ecrecover the signature, derive implicit ownerId,
/// check that the owner_config either allows it via implicit EOA rule or
/// explicit registration.
///
/// For configured mode: parse the verifier type, attempt native verification
/// (K1 / P256), verify the returned ownerId against owner_config, and check
/// SENDER scope.
fn validate_sender(
    tx: &TxEip8130,
    sender: Address,
    state: &dyn reth_storage_api::StateProvider,
) -> Result<B256, Eip8130ValidationError> {
    let parsed =
        parse_sender_auth(tx).map_err(|e| Eip8130ValidationError::SenderAuthInvalid(e.into()))?;
    let sig_hash = sender_signature_hash(tx);

    match parsed {
        ParsedSenderAuth::Eoa { signature } => {
            let sig_bytes = Bytes::copy_from_slice(&signature);
            let result = try_native_verify(VERIFIER_K1, &sig_bytes, sig_hash);
            let owner_id = match result {
                NativeVerifyResult::Verified(id) => id,
                NativeVerifyResult::Invalid(e) => {
                    return Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()));
                }
                NativeVerifyResult::Unsupported => {
                    return Err(Eip8130ValidationError::SenderAuthInvalid(
                        "K1 should be natively supported".into(),
                    ));
                }
            };

            let recovered_address = Address::from_slice(&owner_id.as_slice()[..20]);

            check_owner_authorized(
                state,
                recovered_address,
                owner_id,
                K1_VERIFIER_ADDRESS,
                OwnerScope::SENDER,
                "sender",
            )?;

            Ok(owner_id)
        }
        ParsedSenderAuth::Configured { verifier_type, data } => {
            let target = resolve_verifier(verifier_type, &data)
                .map_err(|e| Eip8130ValidationError::SenderAuthInvalid(e.into()))?;

            let (verifier_address, verify_data) = resolve_target(&target)?;

            let result = try_native_verify(verifier_type, &verify_data, sig_hash);
            match result {
                NativeVerifyResult::Verified(owner_id) => {
                    check_owner_authorized(
                        state,
                        sender,
                        owner_id,
                        verifier_address,
                        OwnerScope::SENDER,
                        "sender",
                    )?;
                    Ok(owner_id)
                }
                NativeVerifyResult::Invalid(e) => {
                    Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()))
                }
                NativeVerifyResult::Unsupported => {
                    // WebAuthn, DELEGATE, or custom verifiers can't be verified
                    // natively. Accept the tx based on structural + nonce + balance
                    // checks; actual STATICCALL is deferred to execution time.
                    Ok(B256::ZERO)
                }
            }
        }
    }
}

/// Validates `payer_auth` for a sponsored AA transaction.
fn validate_payer(
    tx: &TxEip8130,
    payer: Address,
    state: &dyn reth_storage_api::StateProvider,
) -> Result<(), Eip8130ValidationError> {
    if tx.payer_auth.is_empty() {
        return Err(Eip8130ValidationError::PayerAuthInvalid(
            "payer_auth is empty for sponsored tx".into(),
        ));
    }

    let sig_hash = payer_signature_hash(tx);

    let verifier_type = tx.payer_auth[0];
    let data = Bytes::copy_from_slice(&tx.payer_auth[1..]);

    let target = resolve_verifier(verifier_type, &data)
        .map_err(|e| Eip8130ValidationError::PayerAuthInvalid(e.into()))?;

    let (verifier_address, verify_data) = resolve_target_for_payer(&target)?;

    let result = try_native_verify(verifier_type, &verify_data, sig_hash);
    match result {
        NativeVerifyResult::Verified(owner_id) => {
            check_owner_authorized(
                state,
                payer,
                owner_id,
                verifier_address,
                OwnerScope::PAYER,
                "payer",
            )?;
            Ok(())
        }
        NativeVerifyResult::Invalid(e) => Err(Eip8130ValidationError::PayerAuthInvalid(e.to_string())),
        NativeVerifyResult::Unsupported => Ok(()),
    }
}

/// Resolves a `VerifierTarget` into `(verifier_address, verify_data)`.
fn resolve_target(target: &VerifierTarget) -> Result<(Address, Bytes), Eip8130ValidationError> {
    match target {
        VerifierTarget::Native { verifier_type, data } => {
            let addr = verifier_type_to_address(*verifier_type).ok_or_else(|| {
                Eip8130ValidationError::SenderAuthInvalid("unknown native verifier".into())
            })?;
            Ok((addr, data.clone()))
        }
        VerifierTarget::Custom { verifier_address, data } => {
            Ok((*verifier_address, data.clone()))
        }
    }
}

/// Same as `resolve_target` but returns payer-specific errors.
fn resolve_target_for_payer(
    target: &VerifierTarget,
) -> Result<(Address, Bytes), Eip8130ValidationError> {
    match target {
        VerifierTarget::Native { verifier_type, data } => {
            let addr = verifier_type_to_address(*verifier_type).ok_or_else(|| {
                Eip8130ValidationError::PayerAuthInvalid("unknown native verifier".into())
            })?;
            Ok((addr, data.clone()))
        }
        VerifierTarget::Custom { verifier_address, data } => {
            Ok((*verifier_address, data.clone()))
        }
    }
}

/// Checks that the owner_config for `(account, owner_id)` authorizes the given
/// verifier and has the required scope bit.
///
/// Implements the implicit EOA rule: if the slot is empty and
/// `owner_id == bytes32(bytes20(account))`, the K1 verifier is authorized.
fn check_owner_authorized(
    state: &dyn reth_storage_api::StateProvider,
    account: Address,
    owner_id: B256,
    expected_verifier: Address,
    required_scope: u8,
    role: &str,
) -> Result<(), Eip8130ValidationError> {
    let (verifier, scope) = read_owner_config_from_state(state, account, owner_id)?;

    if verifier != Address::ZERO {
        if verifier != expected_verifier {
            return Err(Eip8130ValidationError::SenderNotAuthorized(format!(
                "{role} owner_config verifier mismatch: expected {expected_verifier}, got {verifier}"
            )));
        }
        if scope != 0 && (scope & required_scope) == 0 {
            return Err(Eip8130ValidationError::SenderNotAuthorized(format!(
                "{role} owner lacks required scope bit 0x{required_scope:02x}"
            )));
        }
        return Ok(());
    }

    let implicit_id = implicit_eoa_owner_id(account);
    if owner_id == implicit_id && expected_verifier == K1_VERIFIER_ADDRESS {
        return Ok(());
    }

    Err(Eip8130ValidationError::SenderNotAuthorized(format!(
        "{role} not authorized (no owner_config and implicit EOA rule doesn't apply)"
    )))
}

/// Full AA transaction validation pipeline for the mempool.
///
/// Validates structural integrity, expiry, nonce, sender/payer authorization,
/// and payer balance. Returns the data the txpool needs to order and track
/// the transaction.
pub fn validate_eip8130_transaction<Tx, Client>(
    transaction: &Tx,
    block_timestamp: u64,
    client: &Client,
) -> Result<Eip8130ValidationOutcome, Eip8130ValidationError>
where
    Tx: OpPooledTx + Transaction,
    Client: StateProviderFactory,
{
    let tx = decode_tx_eip8130(transaction)?;

    // 1. Structural validation (no state needed)
    validate_structure(&tx).map_err(Eip8130ValidationError::Structural)?;

    // 2. Expiry check
    validate_expiry(&tx, block_timestamp).map_err(|e| match e {
        ValidationError::Expired { expiry, current } => {
            Eip8130ValidationError::Expired { expiry, current }
        }
        other => Eip8130ValidationError::Structural(other),
    })?;

    // 3. Open state provider for storage reads
    let state = client
        .latest()
        .map_err(|e| Eip8130ValidationError::StateError(e.to_string()))?;

    let sender = tx.effective_sender();

    // 4. Nonce validation
    let nonce_key_slot = nonce_slot(sender, tx.nonce_key);
    let current_nonce =
        read_storage(&*state, NONCE_MANAGER_ADDRESS, nonce_key_slot)?.to::<u64>();
    if current_nonce != tx.nonce_sequence {
        return Err(Eip8130ValidationError::NonceMismatch {
            expected: current_nonce,
            got: tx.nonce_sequence,
        });
    }

    // 5. Sender authorization (includes signature verification for K1/P256)
    validate_sender(&tx, sender, &*state)?;

    // 6. Payer resolution and authorization
    let payer = if tx.payer == Address::ZERO { sender } else { tx.payer };
    if payer != sender {
        validate_payer(&tx, payer, &*state)?;
    }

    // 7. Balance check — payer must cover max gas cost
    let max_gas_cost =
        U256::from(tx.gas_limit).saturating_mul(U256::from(tx.max_fee_per_gas));
    let balance = state
        .account_balance(&payer)
        .map_err(|e| Eip8130ValidationError::StateError(e.to_string()))?
        .unwrap_or_default();
    if balance < max_gas_cost {
        return Err(Eip8130ValidationError::InsufficientBalance {
            required: max_gas_cost,
            available: balance,
        });
    }

    Ok(Eip8130ValidationOutcome { balance, state_nonce: tx.nonce_sequence })
}
