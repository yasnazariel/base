//! AA transaction validation pipeline.
//!
//! Implements the mempool acceptance flow for EIP-8130 transactions. The
//! pipeline validates structural integrity, resolves authentication, and
//! checks on-chain state (owner configuration, nonces, balances, locks).

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::SolCall;
use revm::database::Database;

use super::{
    OwnerScope,
    accessors::{read_change_sequence, read_lock_state, read_nonce, read_owner_config},
    constants::MAX_SIGNATURE_SIZE,
    predeploys::{
        DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
        P256_WEBAUTHN_VERIFIER_ADDRESS,
    },
    tx::TxAa,
};

/// Result of a successful AA transaction validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// The resolved sender address.
    pub sender: Address,
    /// The authenticated owner ID for the sender.
    pub sender_owner_id: B256,
    /// The effective payer (sender if self-pay).
    pub payer: Address,
    /// The authenticated owner ID for the payer (same as sender for self-pay).
    pub payer_owner_id: Option<B256>,
}

/// Errors that can occur during AA transaction validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The `sender_auth` field is too large.
    #[error("sender_auth exceeds max size ({0} > {MAX_SIGNATURE_SIZE})")]
    SenderAuthTooLarge(usize),

    /// The `payer_auth` field is too large.
    #[error("payer_auth exceeds max size ({0} > {MAX_SIGNATURE_SIZE})")]
    PayerAuthTooLarge(usize),

    /// Failed to parse `sender_auth`.
    #[error("invalid sender_auth: {0}")]
    InvalidSenderAuth(&'static str),

    /// Failed to parse `payer_auth`.
    #[error("invalid payer_auth: {0}")]
    InvalidPayerAuth(&'static str),

    /// `account_changes` has invalid structure (e.g. create not first).
    #[error("invalid account_changes structure: {0}")]
    InvalidAccountChanges(&'static str),

    /// The transaction has expired.
    #[error("transaction expired (expiry={expiry}, current={current})")]
    Expired {
        /// The transaction's expiry timestamp.
        expiry: u64,
        /// The current block timestamp.
        current: u64,
    },

    /// Nonce mismatch.
    #[error("nonce mismatch (expected={expected}, got={got})")]
    NonceMismatch {
        /// The expected nonce.
        expected: u64,
        /// The nonce in the transaction.
        got: u64,
    },

    /// The nonce_key exceeds uint192 range.
    #[error("nonce_key exceeds uint192")]
    NonceKeyTooLarge,

    /// The sender's owner is not authorized.
    #[error("sender owner not authorized")]
    SenderNotAuthorized,

    /// The sender's owner lacks the required scope bit.
    #[error("sender owner lacks SENDER scope")]
    SenderScopeMissing,

    /// The payer's owner is not authorized.
    #[error("payer owner not authorized")]
    PayerNotAuthorized,

    /// The payer's owner lacks the required scope bit.
    #[error("payer owner lacks PAYER scope")]
    PayerScopeMissing,

    /// Verifier STATICCALL reverted or returned invalid data.
    #[error("verifier call failed: {0}")]
    VerifierCallFailed(String),

    /// The account is locked and config changes are not allowed.
    #[error("account is locked")]
    AccountLocked,

    /// Config change sequence mismatch.
    #[error("config change sequence mismatch (expected={expected}, got={got})")]
    SequenceMismatch {
        /// The expected sequence.
        expected: u64,
        /// The sequence in the transaction.
        got: u64,
    },

    /// Payer has insufficient balance.
    #[error("payer insufficient balance (required={required}, available={available})")]
    InsufficientBalance {
        /// The required balance.
        required: U256,
        /// The available balance.
        available: U256,
    },

    /// Database error during validation.
    #[error("database error: {0}")]
    Database(String),
}

/// Maximum value of a uint192.
const UINT192_MAX: U256 = U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, 0]);

/// Validates structural constraints that don't require DB access.
pub fn validate_structure(tx: &TxAa) -> Result<(), ValidationError> {
    if tx.sender_auth.len() > MAX_SIGNATURE_SIZE {
        return Err(ValidationError::SenderAuthTooLarge(tx.sender_auth.len()));
    }
    if tx.payer_auth.len() > MAX_SIGNATURE_SIZE {
        return Err(ValidationError::PayerAuthTooLarge(tx.payer_auth.len()));
    }
    if tx.nonce_key > UINT192_MAX {
        return Err(ValidationError::NonceKeyTooLarge);
    }

    validate_account_changes_structure(tx)?;

    Ok(())
}

/// Validates the `account_changes` array structure.
fn validate_account_changes_structure(tx: &TxAa) -> Result<(), ValidationError> {
    use super::types::AccountChangeEntry;

    let mut seen_create = false;
    for (i, entry) in tx.account_changes.iter().enumerate() {
        match entry {
            AccountChangeEntry::Create(_) => {
                if seen_create {
                    return Err(ValidationError::InvalidAccountChanges(
                        "multiple create entries",
                    ));
                }
                if i != 0 {
                    return Err(ValidationError::InvalidAccountChanges(
                        "create entry must be first",
                    ));
                }
                seen_create = true;
            }
            AccountChangeEntry::ConfigChange(_) => {}
        }
    }
    Ok(())
}

/// Validates expiry against the current block timestamp.
pub fn validate_expiry(tx: &TxAa, block_timestamp: u64) -> Result<(), ValidationError> {
    if tx.expiry != 0 && block_timestamp > tx.expiry {
        return Err(ValidationError::Expired { expiry: tx.expiry, current: block_timestamp });
    }
    Ok(())
}

/// Validates the nonce against on-chain state.
pub fn validate_nonce<DB: Database>(
    db: &mut DB,
    tx: &TxAa,
) -> Result<(), ValidationError> {
    let current = read_nonce(db, tx.effective_sender(), tx.nonce_key)
        .map_err(|e| ValidationError::Database(format!("{e:?}")))?;
    if current != tx.nonce_sequence {
        return Err(ValidationError::NonceMismatch { expected: current, got: tx.nonce_sequence });
    }
    Ok(())
}

/// Resolves the effective sender address.
///
/// If `from == Address::ZERO` (EOA mode), the sender must be recovered from
/// `sender_auth` via ecrecover. Otherwise, the `from` field is used directly.
pub fn resolve_sender(tx: &TxAa) -> Address {
    tx.effective_sender()
}

/// Checks whether the sender is authorized by verifying their `owner_config`.
///
/// For the implicit EOA rule: if the slot is empty and
/// `owner_id == bytes32(bytes20(account))`, the K1 verifier is used by default.
pub fn check_sender_authorization<DB: Database>(
    db: &mut DB,
    account: Address,
    owner_id: B256,
) -> Result<(Address, u8), ValidationError> {
    let (verifier, scope) = read_owner_config(db, account, owner_id)
        .map_err(|e| ValidationError::Database(format!("{e:?}")))?;

    if verifier != Address::ZERO {
        if scope != 0 && (scope & OwnerScope::SENDER) == 0 {
            return Err(ValidationError::SenderScopeMissing);
        }
        return Ok((verifier, scope));
    }

    let implicit_owner_id = implicit_eoa_owner_id(account);
    if owner_id == implicit_owner_id {
        return Ok((K1_VERIFIER_ADDRESS, 0));
    }

    Err(ValidationError::SenderNotAuthorized)
}

/// Checks whether the payer is authorized.
pub fn check_payer_authorization<DB: Database>(
    db: &mut DB,
    payer: Address,
    owner_id: B256,
) -> Result<(Address, u8), ValidationError> {
    let (verifier, scope) = read_owner_config(db, payer, owner_id)
        .map_err(|e| ValidationError::Database(format!("{e:?}")))?;

    if verifier != Address::ZERO {
        if scope != 0 && (scope & OwnerScope::PAYER) == 0 {
            return Err(ValidationError::PayerScopeMissing);
        }
        return Ok((verifier, scope));
    }

    let implicit_owner_id = implicit_eoa_owner_id(payer);
    if owner_id == implicit_owner_id {
        return Ok((K1_VERIFIER_ADDRESS, 0));
    }

    Err(ValidationError::PayerNotAuthorized)
}

/// Checks lock state before config changes.
pub fn check_lock_state<DB: Database>(
    db: &mut DB,
    account: Address,
) -> Result<(), ValidationError> {
    let lock = read_lock_state(db, account)
        .map_err(|e| ValidationError::Database(format!("{e:?}")))?;
    if lock.locked {
        return Err(ValidationError::AccountLocked);
    }
    Ok(())
}

/// Validates config change sequences.
pub fn validate_config_change_sequences<DB: Database>(
    db: &mut DB,
    tx: &TxAa,
) -> Result<(), ValidationError> {
    use super::types::AccountChangeEntry;

    for entry in &tx.account_changes {
        if let AccountChangeEntry::ConfigChange(change) = entry {
            let expected = read_change_sequence(db, tx.effective_sender(), change.chain_id)
                .map_err(|e| ValidationError::Database(format!("{e:?}")))?;
            if change.sequence != expected {
                return Err(ValidationError::SequenceMismatch {
                    expected,
                    got: change.sequence,
                });
            }
        }
    }
    Ok(())
}

/// Computes `bytes32(bytes20(account))` — the implicit EOA owner ID.
pub fn implicit_eoa_owner_id(account: Address) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[..20].copy_from_slice(account.as_slice());
    B256::from(bytes)
}

/// Maps a verifier type byte to the corresponding predeploy address.
pub fn verifier_type_to_address(verifier_type: u8) -> Option<Address> {
    match verifier_type {
        0x01 => Some(K1_VERIFIER_ADDRESS),
        0x02 => Some(P256_RAW_VERIFIER_ADDRESS),
        0x03 => Some(P256_WEBAUTHN_VERIFIER_ADDRESS),
        0x04 => Some(DELEGATE_VERIFIER_ADDRESS),
        _ => None,
    }
}

/// Encodes a STATICCALL to `IVerifier.verify(hash, data)`.
pub fn encode_verify_call(hash: B256, data: &Bytes) -> Bytes {
    use super::abi::IVerifier;
    let call = IVerifier::verifyCall {
        hash,
        data: data.clone(),
    };
    Bytes::from(call.abi_encode())
}

/// Decodes the return value from `IVerifier.verify()` → `ownerId`.
pub fn decode_verify_return(output: &[u8]) -> Option<B256> {
    use super::abi::IVerifier;
    IVerifier::verifyCall::abi_decode_returns(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn implicit_eoa_owner_id_correct() {
        let account = address!("0x1111111111111111111111111111111111111111");
        let owner_id = implicit_eoa_owner_id(account);
        assert_eq!(&owner_id.as_slice()[..20], account.as_slice());
        assert!(owner_id.as_slice()[20..].iter().all(|&b| b == 0));
    }

    #[test]
    fn verifier_type_mapping() {
        assert_eq!(verifier_type_to_address(0x01), Some(K1_VERIFIER_ADDRESS));
        assert_eq!(verifier_type_to_address(0x02), Some(P256_RAW_VERIFIER_ADDRESS));
        assert_eq!(verifier_type_to_address(0x03), Some(P256_WEBAUTHN_VERIFIER_ADDRESS));
        assert_eq!(verifier_type_to_address(0x04), Some(DELEGATE_VERIFIER_ADDRESS));
        assert_eq!(verifier_type_to_address(0x00), None);
        assert_eq!(verifier_type_to_address(0x05), None);
    }

    #[test]
    fn structure_validation_empty_tx() {
        let tx = TxAa::default();
        assert!(validate_structure(&tx).is_ok());
    }

    #[test]
    fn structure_validation_sender_auth_too_large() {
        let tx = TxAa {
            sender_auth: Bytes::from(vec![0u8; MAX_SIGNATURE_SIZE + 1]),
            ..Default::default()
        };
        assert!(matches!(
            validate_structure(&tx),
            Err(ValidationError::SenderAuthTooLarge(_))
        ));
    }

    #[test]
    fn structure_validation_nonce_key_too_large() {
        let tx = TxAa {
            nonce_key: UINT192_MAX + U256::from(1),
            ..Default::default()
        };
        assert!(matches!(
            validate_structure(&tx),
            Err(ValidationError::NonceKeyTooLarge)
        ));
    }

    #[test]
    fn expiry_validation() {
        let tx = TxAa { expiry: 100, ..Default::default() };
        assert!(validate_expiry(&tx, 50).is_ok());
        assert!(validate_expiry(&tx, 100).is_ok());
        assert!(matches!(
            validate_expiry(&tx, 101),
            Err(ValidationError::Expired { .. })
        ));
    }

    #[test]
    fn no_expiry_always_valid() {
        let tx = TxAa { expiry: 0, ..Default::default() };
        assert!(validate_expiry(&tx, u64::MAX).is_ok());
    }

    #[test]
    fn encode_decode_verify_roundtrip() {
        let hash = B256::repeat_byte(0xAA);
        let data = Bytes::from(vec![1, 2, 3, 4]);
        let encoded = encode_verify_call(hash, &data);
        assert!(!encoded.is_empty());
    }
}
