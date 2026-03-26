//! Proper mempool validation for EIP-8130 (AA) transactions.
//!
//! Validates nonce, expiry, sender/payer authorization (with native Rust
//! crypto for K1 and P256 verifiers), and payer balance before accepting
//! an AA transaction into the pending pool.
//!
//! Custom verifiers (type `0x00`) are verified via an EVM STATICCALL to
//! the verifier contract. This ensures no unverified transactions enter
//! the mempool.

use std::collections::HashSet;

use alloy_consensus::Transaction;
use alloy_primitives::{Address, B256, Bytes, U256};
use reth_storage_api::StateProviderFactory;

use base_alloy_consensus::{
    ACCOUNT_CONFIG_ADDRESS, AccountChangeEntry, DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS,
    NONCE_MANAGER_ADDRESS, NativeVerifyResult, OwnerScope, ParsedSenderAuth,
    P256_RAW_VERIFIER_ADDRESS, P256_WEBAUTHN_VERIFIER_ADDRESS, TxEip8130, ValidationError,
    VerifierTarget, VERIFIER_CUSTOM, VERIFIER_K1, encode_verify_call, implicit_eoa_owner_id,
    intrinsic_gas, lock_slot, nonce_slot, owner_config_slot, parse_owner_config,
    parse_sender_auth, payer_signature_hash, read_sequence, resolve_verifier,
    sender_signature_hash, sequence_base_slot, try_native_verify, validate_expiry,
    validate_structure, verifier_type_to_address,
};

use crate::{InvalidationKey, OpPooledTx, compute_invalidation_keys};

/// Controls which verifier contracts the mempool will accept in AA transactions.
///
/// - `None` (default): all verifiers are accepted.
/// - `Some(set)`: the set contains allowed verifier addresses. Native verifier
///   addresses (K1, P256, WebAuthn, Delegate) are always included automatically.
///   Only custom verifiers (type `0x00`) need explicit allowlisting.
#[derive(Debug, Clone)]
pub struct VerifierAllowlist {
    allowed: HashSet<Address>,
}

impl VerifierAllowlist {
    /// Creates an allowlist from custom verifier addresses.
    ///
    /// The four native verifier addresses are added automatically.
    pub fn new(custom_addresses: impl IntoIterator<Item = Address>) -> Self {
        let mut allowed: HashSet<Address> = custom_addresses.into_iter().collect();
        allowed.insert(K1_VERIFIER_ADDRESS);
        allowed.insert(P256_RAW_VERIFIER_ADDRESS);
        allowed.insert(P256_WEBAUTHN_VERIFIER_ADDRESS);
        allowed.insert(DELEGATE_VERIFIER_ADDRESS);
        Self { allowed }
    }

    /// Returns `true` if the given verifier address is allowed.
    pub fn is_allowed(&self, address: &Address) -> bool {
        self.allowed.contains(address)
    }
}

/// Successful AA validation outcome, providing the data the txpool needs for
/// ordering and balance tracking.
#[derive(Debug)]
pub struct Eip8130ValidationOutcome {
    /// Payer's balance (used for txpool cost checks).
    pub balance: U256,
    /// The sender's current nonce_sequence (used for txpool nonce ordering).
    pub state_nonce: u64,
    /// The resolved sender owner ID (from native signature verification).
    pub sender_owner_id: B256,
    /// Storage slot dependencies for invalidation tracking.
    pub invalidation_keys: HashSet<InvalidationKey>,
    /// The resolved payer address. `None` for self-pay transactions.
    /// Used for payer pending count tracking.
    pub sponsored_payer: Option<Address>,
}

/// Errors from AA transaction validation.
#[derive(Debug)]
pub enum Eip8130ValidationError {
    /// Failed to decode the `TxEip8130` from 2718-encoded bytes.
    DecodeFailed(String),
    /// Structural validation failed (sizes, nonce_key range, account_changes).
    Structural(ValidationError),
    /// Transaction chain_id does not match the network.
    ChainIdMismatch {
        /// Network chain_id.
        expected: u64,
        /// Transaction's chain_id.
        got: u64,
    },
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
    /// Verifier address is not on the mempool allowlist.
    VerifierNotAllowed(Address),
    /// Custom verifier STATICCALL failed in the txpool EVM.
    CustomVerifierCallFailed(String),
    /// Custom verifier has EIP-7702 delegation bytecode prefix.
    VerifierEip7702Delegated(Address),
    /// Account is locked; config changes are rejected.
    AccountLocked,
    /// Config change sequence does not match on-chain value.
    SequenceMismatch {
        /// On-chain sequence.
        expected: u64,
        /// Sequence in the transaction.
        got: u64,
    },
    /// Gas limit is below the intrinsic gas cost.
    IntrinsicGasTooLow {
        /// Minimum required gas.
        intrinsic: u64,
        /// Gas limit in the transaction.
        gas_limit: u64,
    },
    /// Payer has insufficient balance to cover `gas_limit * max_fee_per_gas`.
    InsufficientBalance {
        /// Required balance.
        required: U256,
        /// Available balance.
        available: U256,
    },
    /// Payer has too many pending sponsored transactions.
    PayerPendingLimitExceeded {
        /// The payer address.
        payer: Address,
        /// Current pending count.
        count: usize,
        /// Maximum allowed.
        limit: usize,
    },
    /// Error reading on-chain state.
    StateError(String),
}

impl std::fmt::Display for Eip8130ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DecodeFailed(e) => write!(f, "decode failed: {e}"),
            Self::Structural(e) => write!(f, "structural: {e}"),
            Self::ChainIdMismatch { expected, got } => {
                write!(f, "chain_id mismatch (expected={expected}, got={got})")
            }
            Self::Expired { expiry, current } => {
                write!(f, "expired (expiry={expiry}, current={current})")
            }
            Self::NonceMismatch { expected, got } => {
                write!(f, "nonce mismatch (expected={expected}, got={got})")
            }
            Self::VerifierNotAllowed(addr) => write!(f, "verifier {addr} not on allowlist"),
            Self::CustomVerifierCallFailed(e) => {
                write!(f, "custom verifier STATICCALL failed: {e}")
            }
            Self::VerifierEip7702Delegated(addr) => {
                write!(f, "verifier {addr} has EIP-7702 delegation prefix")
            }
            Self::SenderAuthInvalid(e) => write!(f, "sender auth invalid: {e}"),
            Self::SenderNotAuthorized(e) => write!(f, "sender not authorized: {e}"),
            Self::PayerAuthInvalid(e) => write!(f, "payer auth invalid: {e}"),
            Self::PayerNotAuthorized(e) => write!(f, "payer not authorized: {e}"),
            Self::AccountLocked => write!(f, "account is locked"),
            Self::SequenceMismatch { expected, got } => {
                write!(f, "config change sequence mismatch (expected={expected}, got={got})")
            }
            Self::IntrinsicGasTooLow { intrinsic, gas_limit } => {
                write!(f, "gas limit below intrinsic (intrinsic={intrinsic}, limit={gas_limit})")
            }
            Self::InsufficientBalance { required, available } => {
                write!(f, "payer insufficient balance (required={required}, available={available})")
            }
            Self::PayerPendingLimitExceeded { payer, count, limit } => {
                write!(f, "payer {payer} pending limit exceeded ({count}/{limit})")
            }
            Self::StateError(e) => write!(f, "state access error: {e}"),
        }
    }
}

impl std::error::Error for Eip8130ValidationError {}

impl reth_transaction_pool::error::PoolTransactionError for Eip8130ValidationError {
    fn is_bad_transaction(&self) -> bool {
        matches!(
            self,
            Self::Structural(_)
                | Self::DecodeFailed(_)
                | Self::ChainIdMismatch { .. }
                | Self::CustomVerifierCallFailed(_)
                | Self::VerifierEip7702Delegated(_)
        )
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Extracts `TxEip8130` from a pool transaction, avoiding re-encode/re-decode.
fn decode_tx_eip8130<Tx: OpPooledTx>(transaction: &Tx) -> Result<TxEip8130, Eip8130ValidationError> {
    transaction
        .as_eip8130()
        .cloned()
        .ok_or_else(|| Eip8130ValidationError::DecodeFailed("not an AA transaction".into()))
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

/// Extracts the custom verifier address from an auth blob, if present.
///
/// Returns `Some(address)` when the first byte is `VERIFIER_CUSTOM` (0x00) and
/// at least 20 address bytes follow. Returns `None` for native verifier types,
/// empty blobs, or blobs that are too short for a valid custom verifier.
fn extract_custom_verifier_address(auth: &Bytes) -> Option<Address> {
    if auth.is_empty() {
        return None;
    }
    if auth[0] != VERIFIER_CUSTOM {
        return None;
    }
    // 1 byte type + at least 20 bytes address
    if auth.len() < 21 {
        return None;
    }
    Some(Address::from_slice(&auth[1..21]))
}

/// Resolves the sender address for an AA transaction.
///
/// For EOA mode (`from == Address::ZERO`): ecrecovers the sender from
/// `sender_auth` and returns the recovered address plus the owner_id.
///
/// For configured mode: returns `tx.from` as the sender. The owner_id
/// is not yet validated (done in `validate_sender_authorization`).
fn resolve_sender_address(tx: &TxEip8130) -> Result<(Address, B256), Eip8130ValidationError> {
    let parsed =
        parse_sender_auth(tx).map_err(|e| Eip8130ValidationError::SenderAuthInvalid(e.into()))?;

    match parsed {
        ParsedSenderAuth::Eoa { signature } => {
            let sig_hash = sender_signature_hash(tx);
            let sig_bytes = Bytes::copy_from_slice(&signature);
            let result = try_native_verify(VERIFIER_K1, &sig_bytes, sig_hash);
            match result {
                NativeVerifyResult::Verified(owner_id) => {
                    let recovered = Address::from_slice(&owner_id.as_slice()[..20]);
                    Ok((recovered, owner_id))
                }
                NativeVerifyResult::Invalid(e) => {
                    Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()))
                }
                NativeVerifyResult::Unsupported => Err(
                    Eip8130ValidationError::SenderAuthInvalid(
                        "K1 should be natively supported".into(),
                    ),
                ),
            }
        }
        ParsedSenderAuth::Configured { .. } => Ok((tx.from, B256::ZERO)),
    }
}

/// Default gas limit for custom verifier STATICCALLs in the txpool.
///
/// Caps how much gas a custom verifier contract can consume during
/// mempool validation. Override via
/// [`OpTransactionValidator::with_custom_verifier_gas_limit`].
pub const DEFAULT_CUSTOM_VERIFIER_GAS_LIMIT: u64 = 100_000;

/// Executes a custom verifier's `IVerifier.verify(hash, data)` via a
/// lightweight EVM STATICCALL and validates the returned owner_id against
/// the on-chain owner_config.
///
/// Returns the authenticated `owner_id` on success.
fn verify_custom_via_evm(
    state: &dyn reth_storage_api::StateProvider,
    verifier: Address,
    sig_hash: B256,
    auth_data: &Bytes,
    account: Address,
    required_scope: u8,
    role: OwnerRole,
    gas_limit: u64,
) -> Result<B256, Eip8130ValidationError> {
    use reth_revm::database::StateProviderDatabase;
    use revm::{
        Context, ExecuteEvm, MainBuilder, MainContext,
        context::TxEnv,
        database::CacheDB,
        primitives::TxKind,
    };

    let calldata = encode_verify_call(sig_hash, auth_data);

    let db = CacheDB::new(StateProviderDatabase::new(state));
    let tx = TxEnv::builder()
        .caller(account)
        .kind(TxKind::Call(verifier))
        .data(calldata)
        .gas_limit(gas_limit)
        .build()
        .map_err(|e| Eip8130ValidationError::CustomVerifierCallFailed(format!("{e:?}")))?;

    let ctx = Context::mainnet().with_db(db).with_tx(tx);
    let mut evm = ctx.build_mainnet();

    let exec_result = evm.replay().map_err(|e| {
        Eip8130ValidationError::CustomVerifierCallFailed(format!("{e:?}"))
    })?;

    if !exec_result.result.is_success() {
        return Err(role.not_authorized(
            "custom verifier STATICCALL reverted".into(),
        ));
    }

    let output = exec_result.result.output().ok_or_else(|| {
        role.not_authorized("custom verifier returned no output".into())
    })?;

    if output.len() < 32 {
        return Err(role.not_authorized(format!(
            "custom verifier returned {} bytes, expected >= 32",
            output.len()
        )));
    }

    let owner_id = B256::from_slice(&output[..32]);

    check_owner_authorized(
        state,
        account,
        owner_id,
        verifier,
        required_scope,
        role,
    )?;

    Ok(owner_id)
}

/// Validates `sender_auth` authorization against on-chain owner_config.
///
/// For EOA mode: checks the already-recovered `owner_id` against AccountConfig.
/// For configured mode: parses the verifier, attempts native verification,
/// and checks the owner_config for SENDER scope.
fn validate_sender_authorization(
    tx: &TxEip8130,
    sender: Address,
    eoa_owner_id: B256,
    state: &dyn reth_storage_api::StateProvider,
    custom_verifier_gas_limit: u64,
) -> Result<B256, Eip8130ValidationError> {
    if tx.is_eoa() {
        check_owner_authorized(
            state,
            sender,
            eoa_owner_id,
            K1_VERIFIER_ADDRESS,
            OwnerScope::SENDER,
            OwnerRole::Sender,
        )?;
        return Ok(eoa_owner_id);
    }

    let parsed =
        parse_sender_auth(tx).map_err(|e| Eip8130ValidationError::SenderAuthInvalid(e.into()))?;
    let sig_hash = sender_signature_hash(tx);

    match parsed {
        ParsedSenderAuth::Eoa { .. } => unreachable!("handled above"),
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
                        OwnerRole::Sender,
                    )?;
                    Ok(owner_id)
                }
                NativeVerifyResult::Invalid(e) => {
                    Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()))
                }
                NativeVerifyResult::Unsupported => {
                    verify_custom_via_evm(
                        state,
                        verifier_address,
                        sig_hash,
                        &verify_data,
                        sender,
                        OwnerScope::SENDER,
                        OwnerRole::Sender,
                        custom_verifier_gas_limit,
                    )
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
    custom_verifier_gas_limit: u64,
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
                OwnerRole::Payer,
            )?;
            Ok(())
        }
        NativeVerifyResult::Invalid(e) => Err(Eip8130ValidationError::PayerAuthInvalid(e.to_string())),
        NativeVerifyResult::Unsupported => {
            verify_custom_via_evm(
                state,
                verifier_address,
                sig_hash,
                &verify_data,
                payer,
                OwnerScope::PAYER,
                OwnerRole::Payer,
                custom_verifier_gas_limit,
            )?;
            Ok(())
        }
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
    role: OwnerRole,
) -> Result<(), Eip8130ValidationError> {
    let (verifier, scope) = read_owner_config_from_state(state, account, owner_id)?;

    if verifier != Address::ZERO {
        if verifier != expected_verifier {
            return Err(role.not_authorized(format!(
                "owner_config verifier mismatch: expected {expected_verifier}, got {verifier}"
            )));
        }
        if scope != 0 && (scope & required_scope) == 0 {
            return Err(role.not_authorized(format!(
                "owner lacks required scope bit 0x{required_scope:02x}"
            )));
        }
        return Ok(());
    }

    let implicit_id = implicit_eoa_owner_id(account);
    if owner_id == implicit_id && expected_verifier == K1_VERIFIER_ADDRESS {
        return Ok(());
    }

    Err(role.not_authorized(
        "no owner_config and implicit EOA rule doesn't apply".into(),
    ))
}

/// Distinguishes between sender and payer roles for error reporting.
#[derive(Debug, Clone, Copy)]
enum OwnerRole {
    Sender,
    Payer,
}

impl OwnerRole {
    fn not_authorized(self, detail: String) -> Eip8130ValidationError {
        match self {
            Self::Sender => Eip8130ValidationError::SenderNotAuthorized(detail),
            Self::Payer => Eip8130ValidationError::PayerNotAuthorized(detail),
        }
    }
}

/// Full AA transaction validation pipeline for the mempool.
///
/// Validates structural integrity, expiry, chain_id, nonce, sender/payer
/// authorization, and payer balance. Returns the data the txpool needs to
/// order and track the transaction.
pub fn validate_eip8130_transaction<Tx, Client>(
    transaction: &Tx,
    block_timestamp: u64,
    chain_id: u64,
    client: &Client,
    verifier_allowlist: Option<&VerifierAllowlist>,
    custom_verifier_gas_limit: u64,
) -> Result<Eip8130ValidationOutcome, Eip8130ValidationError>
where
    Tx: OpPooledTx + Transaction,
    Client: StateProviderFactory,
{
    let tx = decode_tx_eip8130(transaction)?;

    // 1. Structural validation (no state needed)
    validate_structure(&tx).map_err(Eip8130ValidationError::Structural)?;

    // 1b. Chain ID must match the network.
    if tx.chain_id != chain_id {
        return Err(Eip8130ValidationError::ChainIdMismatch {
            expected: chain_id,
            got: tx.chain_id,
        });
    }

    // 2. Expiry check
    validate_expiry(&tx, block_timestamp).map_err(|e| match e {
        ValidationError::Expired { expiry, current } => {
            Eip8130ValidationError::Expired { expiry, current }
        }
        other => Eip8130ValidationError::Structural(other),
    })?;

    // 2b. Verifier allowlist — reject custom verifiers not on the list.
    //     Native types (0x01–0x04) are always allowed. Only custom verifier
    //     addresses (type 0x00) are checked against the allowlist.
    if let Some(allowlist) = verifier_allowlist {
        if !tx.is_eoa() {
            if let Some(addr) = extract_custom_verifier_address(&tx.sender_auth) {
                if !allowlist.is_allowed(&addr) {
                    return Err(Eip8130ValidationError::VerifierNotAllowed(addr));
                }
            }
        }
        if tx.payer != Address::ZERO && tx.payer != tx.effective_sender() {
            if let Some(addr) = extract_custom_verifier_address(&tx.payer_auth) {
                if !allowlist.is_allowed(&addr) {
                    return Err(Eip8130ValidationError::VerifierNotAllowed(addr));
                }
            }
        }
    }

    // 3. Resolve the sender address. For EOA mode (`from == Address::ZERO`),
    //    ecrecover derives the real sender. This must happen before any state
    //    reads that key on the sender address (nonce, lock, sequence, balance).
    let (sender, eoa_owner_id) = resolve_sender_address(&tx)?;

    // 4. Open state provider for storage reads
    let state = client
        .latest()
        .map_err(|e| Eip8130ValidationError::StateError(e.to_string()))?;

    // 4b. Reject custom verifiers whose bytecode starts with the EIP-7702
    //     delegation designator (0xef0100). Delegated accounts must not be
    //     used as verifier contracts.
    for auth_blob in [&tx.sender_auth, &tx.payer_auth] {
        if let Some(addr) = extract_custom_verifier_address(auth_blob) {
            if let Ok(Some(code)) = state.account_code(&addr) {
                if code.original_bytes().starts_with(&[0xef, 0x01, 0x00]) {
                    return Err(Eip8130ValidationError::VerifierEip7702Delegated(addr));
                }
            }
        }
    }

    // 5. Nonce validation
    let nonce_key_slot = nonce_slot(sender, tx.nonce_key);
    let current_nonce =
        read_storage(&*state, NONCE_MANAGER_ADDRESS, nonce_key_slot)?.to::<u64>();
    if current_nonce != tx.nonce_sequence {
        return Err(Eip8130ValidationError::NonceMismatch {
            expected: current_nonce,
            got: tx.nonce_sequence,
        });
    }

    // 6. Lock state — reject config changes on locked accounts
    let has_config_changes = tx
        .account_changes
        .iter()
        .any(|e| matches!(e, AccountChangeEntry::ConfigChange(_)));
    if has_config_changes {
        let lock_slot_key = lock_slot(sender);
        let lock_value = read_storage(&*state, ACCOUNT_CONFIG_ADDRESS, lock_slot_key)?;
        let lock_bytes = lock_value.to_be_bytes::<32>();
        if lock_bytes[0] != 0 {
            return Err(Eip8130ValidationError::AccountLocked);
        }
    }

    // 7. Config change sequence validation
    for entry in &tx.account_changes {
        if let AccountChangeEntry::ConfigChange(change) = entry {
            let seq_slot = sequence_base_slot(sender);
            let packed = read_storage(&*state, ACCOUNT_CONFIG_ADDRESS, seq_slot)?;
            let is_multichain = change.chain_id == 0;
            let expected = read_sequence(packed, is_multichain);
            if change.sequence != expected {
                return Err(Eip8130ValidationError::SequenceMismatch {
                    expected,
                    got: change.sequence,
                });
            }
        }
    }

    // 8. Intrinsic gas — gas_limit must cover the minimum execution cost.
    //    Nonce key is "warm" if the current sequence > 0 (channel already used).
    let nonce_key_is_warm = current_nonce > 0;
    let min_gas = intrinsic_gas(&tx, nonce_key_is_warm, tx.chain_id);
    if tx.gas_limit < min_gas {
        return Err(Eip8130ValidationError::IntrinsicGasTooLow {
            intrinsic: min_gas,
            gas_limit: tx.gas_limit,
        });
    }

    // 9. Sender authorization (checks owner_config for the resolved sender)
    let sender_owner_id = validate_sender_authorization(
        &tx, sender, eoa_owner_id, &*state, custom_verifier_gas_limit,
    )?;

    // 10. Payer resolution and authorization
    let payer = if tx.is_self_pay() { sender } else { tx.payer };
    if payer != sender {
        validate_payer(&tx, payer, &*state, custom_verifier_gas_limit)?;
    }

    // 11. Balance check — payer must cover max gas cost
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

    // 12. Compute invalidation keys for the state-diff based eviction index
    let invalidation_keys = compute_invalidation_keys(
        &tx,
        Some(sender_owner_id).filter(|id| *id != B256::ZERO),
        None,
    );

    let sponsored_payer = if payer != sender { Some(payer) } else { None };

    Ok(Eip8130ValidationOutcome {
        balance,
        state_nonce: tx.nonce_sequence,
        sender_owner_id,
        invalidation_keys,
        sponsored_payer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_includes_native_verifiers() {
        let allowlist = VerifierAllowlist::new(std::iter::empty());
        assert!(allowlist.is_allowed(&K1_VERIFIER_ADDRESS));
        assert!(allowlist.is_allowed(&P256_RAW_VERIFIER_ADDRESS));
        assert!(allowlist.is_allowed(&P256_WEBAUTHN_VERIFIER_ADDRESS));
        assert!(allowlist.is_allowed(&DELEGATE_VERIFIER_ADDRESS));
    }

    #[test]
    fn allowlist_rejects_unknown_custom() {
        let allowlist = VerifierAllowlist::new(std::iter::empty());
        let unknown = Address::repeat_byte(0xAB);
        assert!(!allowlist.is_allowed(&unknown));
    }

    #[test]
    fn allowlist_accepts_configured_custom() {
        let custom = Address::repeat_byte(0xAB);
        let allowlist = VerifierAllowlist::new([custom]);
        assert!(allowlist.is_allowed(&custom));
    }

    #[test]
    fn extract_custom_verifier_from_auth() {
        let mut auth = vec![VERIFIER_CUSTOM];
        let addr = Address::repeat_byte(0xCC);
        auth.extend_from_slice(addr.as_slice());
        auth.extend_from_slice(&[0xDD; 32]);
        let result = extract_custom_verifier_address(&Bytes::from(auth));
        assert_eq!(result, Some(addr));
    }

    #[test]
    fn extract_returns_none_for_native_type() {
        let auth = Bytes::from(vec![VERIFIER_K1, 0xAA, 0xBB]);
        assert_eq!(extract_custom_verifier_address(&auth), None);
    }

    #[test]
    fn extract_returns_none_for_empty() {
        assert_eq!(extract_custom_verifier_address(&Bytes::new()), None);
    }

    #[test]
    fn extract_returns_none_for_short_custom() {
        let auth = Bytes::from(vec![VERIFIER_CUSTOM, 0x01, 0x02]);
        assert_eq!(extract_custom_verifier_address(&auth), None);
    }
}
