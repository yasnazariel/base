//! Proper mempool validation for EIP-8130 (AA) transactions.
//!
//! Validates nonce, expiry, sender/payer authorization (with native Rust
//! crypto for K1 and P256 verifiers), and payer balance before accepting
//! an AA transaction into the pending pool.
//!
//! Custom (non-native) verifiers are verified via an EVM STATICCALL to
//! the verifier contract. This ensures no unverified transactions enter
//! the mempool.

use std::collections::HashSet;

use alloy_consensus::Transaction;
use alloy_primitives::{Address, B256, Bytes, U256};
use reth_storage_api::StateProviderFactory;

use base_alloy_consensus::{
    ACCOUNT_CONFIG_ADDRESS, AccountChangeEntry, CUSTOM_VERIFIER_GAS_CAP, DELEGATE_VERIFIER_ADDRESS,
    K1_VERIFIER_ADDRESS, MAX_CONFIG_OPS_PER_TX, NONCE_FREE_MAX_EXPIRY_WINDOW, NONCE_KEY_MAX,
    NONCE_MANAGER_ADDRESS, NativeVerifyResult, OwnerScope, P256_RAW_VERIFIER_ADDRESS,
    P256_WEBAUTHN_VERIFIER_ADDRESS, ParsedSenderAuth, REVOKED_VERIFIER, TxEip8130,
    ValidationError, config_change_digest, encode_verify_call, expiring_seen_slot,
    implicit_eoa_owner_id, intrinsic_gas, is_native_verifier, lock_slot, nonce_slot,
    owner_config_slot, parse_owner_config, parse_sender_auth, payer_signature_hash, read_sequence,
    sender_signature_hash, sequence_base_slot, try_native_verify, validate_expiry,
    validate_structure,
};

use crate::{InvalidationKey, OpPooledTx, SenderThroughputTier, compute_invalidation_keys};

/// Controls which verifier contracts the mempool will accept in AA transactions.
///
/// - `None` (default): all verifiers are accepted.
/// - `Some(set)`: the set contains allowed verifier addresses. Native verifier
///   addresses (K1, P256, WebAuthn, Delegate) are always included automatically.
///   Only non-native (custom) verifiers need explicit allowlisting.
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
    /// The nonce key from the transaction. Used by the 2D nonce pool to
    /// route transactions with `nonce_key != 0` to the dedicated pool.
    pub nonce_key: U256,
    /// The resolved sender owner ID (from native signature verification).
    pub sender_owner_id: B256,
    /// Storage slot dependencies for invalidation tracking.
    pub invalidation_keys: HashSet<InvalidationKey>,
    /// The resolved payer address. `None` for self-pay transactions.
    /// Used for payer pending count tracking.
    pub sponsored_payer: Option<Address>,
    /// Throughput tier for the sender, derived from the sender's lock state
    /// and the payer's bytecode trustworthiness.
    pub sender_throughput_tier: SenderThroughputTier,
}

/// Errors from AA transaction validation.
#[derive(Debug)]
pub enum Eip8130ValidationError {
    /// Failed to decode the `TxEip8130` from 2718-encoded bytes.
    DecodeFailed(String),
    /// AA transaction exceeds the txpool ingress encoded size cap.
    TxTooLarge {
        /// Encoded transaction size in bytes.
        size: usize,
        /// Maximum allowed encoded size in bytes.
        limit: usize,
    },
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
    /// Config change authorizer auth is invalid.
    AuthorizerAuthInvalid(String),
    /// Config change authorizer lacks CONFIG scope or is not a recognized owner.
    AuthorizerNotAuthorized(String),
    /// Too many config operations in a single transaction.
    TooManyConfigOperations {
        /// Number of operations in the transaction.
        count: usize,
        /// Maximum allowed.
        limit: usize,
    },
    /// Account is locked; config changes are rejected.
    AccountLocked,
    /// AccountConfiguration contract has not been deployed yet.
    AccountConfigNotDeployed,
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
    /// Nonce-free transaction's expiry is too far in the future.
    NonceFreeExpiryTooFar {
        /// Transaction's expiry timestamp.
        expiry: u64,
        /// Maximum allowed expiry.
        max_allowed: u64,
    },
    /// Nonce-free transaction hash already recorded in the on-chain seen set.
    NonceFreeReplay,
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
            Self::TxTooLarge { size, limit } => {
                write!(f, "AA tx too large (size={size}, limit={limit})")
            }
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
            Self::AuthorizerAuthInvalid(e) => write!(f, "authorizer auth invalid: {e}"),
            Self::AuthorizerNotAuthorized(e) => write!(f, "authorizer not authorized: {e}"),
            Self::TooManyConfigOperations { count, limit } => {
                write!(f, "too many config operations ({count}/{limit})")
            }
            Self::AccountLocked => write!(f, "account is locked"),
            Self::AccountConfigNotDeployed => {
                write!(f, "AccountConfiguration contract not deployed")
            }
            Self::SequenceMismatch { expected, got } => {
                write!(f, "config change sequence mismatch (expected={expected}, got={got})")
            }
            Self::IntrinsicGasTooLow { intrinsic, gas_limit } => {
                write!(f, "gas limit below intrinsic (intrinsic={intrinsic}, limit={gas_limit})")
            }
            Self::InsufficientBalance { required, available } => {
                write!(f, "payer insufficient balance (required={required}, available={available})")
            }
            Self::NonceFreeExpiryTooFar { expiry, max_allowed } => {
                write!(f, "nonce-free expiry too far: expiry={expiry}, max_allowed={max_allowed}")
            }
            Self::NonceFreeReplay => {
                write!(f, "nonce-free transaction replay: hash already seen")
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
                | Self::TxTooLarge { .. }
                | Self::ChainIdMismatch { .. }
                | Self::CustomVerifierCallFailed(_)
                | Self::VerifierEip7702Delegated(_)
                | Self::SenderAuthInvalid(_)
                | Self::PayerAuthInvalid(_)
                | Self::AuthorizerAuthInvalid(_)
                | Self::TooManyConfigOperations { .. }
        )
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Extracts `TxEip8130` from a pool transaction, avoiding re-encode/re-decode.
fn decode_tx_eip8130<Tx: OpPooledTx>(
    transaction: &Tx,
) -> Result<TxEip8130, Eip8130ValidationError> {
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
            let result = try_native_verify(K1_VERIFIER_ADDRESS, &sig_bytes, sig_hash);
            match result {
                NativeVerifyResult::Verified(owner_id) => {
                    let recovered = Address::from_slice(&owner_id.as_slice()[..20]);
                    Ok((recovered, owner_id))
                }
                NativeVerifyResult::Invalid(e) => {
                    Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()))
                }
                NativeVerifyResult::Unsupported => Err(Eip8130ValidationError::SenderAuthInvalid(
                    "K1 should be natively supported".into(),
                )),
            }
        }
        ParsedSenderAuth::Configured { .. } => Ok((tx.from, B256::ZERO)),
    }
}

/// Default gas limit for custom verifier STATICCALLs in the txpool.
///
/// Matches [`CUSTOM_VERIFIER_GAS_CAP`] from the consensus layer.
/// Override via [`OpTransactionValidator::with_custom_verifier_gas_limit`].
pub const DEFAULT_CUSTOM_VERIFIER_GAS_LIMIT: u64 = CUSTOM_VERIFIER_GAS_CAP;

/// Maximum EIP-2718 encoded size accepted for a single AA transaction at txpool ingress.
///
/// This is a pragmatic first guard for parse/allocation amplification: oversized
/// AA envelopes are rejected before running deeper stateful validation.
pub const MAX_AA_TX_ENCODED_BYTES: usize = 128 * 1024;

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
        Context, ExecuteEvm, MainBuilder, MainContext, context::TxEnv, database::CacheDB,
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

    let mut ctx = Context::mainnet().with_db(db).with_tx(tx);
    ctx.cfg.disable_nonce_check = true;
    let mut evm = ctx.build_mainnet();

    let exec_result = evm
        .replay()
        .map_err(|e| Eip8130ValidationError::CustomVerifierCallFailed(format!("{e:?}")))?;

    if !exec_result.result.is_success() {
        return Err(role.not_authorized("custom verifier STATICCALL reverted".into()));
    }

    let output = exec_result
        .result
        .output()
        .ok_or_else(|| role.not_authorized("custom verifier returned no output".into()))?;

    if output.len() < 32 {
        return Err(role.not_authorized(format!(
            "custom verifier returned {} bytes, expected >= 32",
            output.len()
        )));
    }

    let owner_id = B256::from_slice(&output[..32]);

    check_owner_authorized(state, account, owner_id, verifier, required_scope, role)?;

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
        ParsedSenderAuth::Configured { verifier, data } => {
            let result = try_native_verify(verifier, &data, sig_hash);
            match result {
                NativeVerifyResult::Verified(owner_id) => {
                    check_owner_authorized(
                        state,
                        sender,
                        owner_id,
                        verifier,
                        OwnerScope::SENDER,
                        OwnerRole::Sender,
                    )?;
                    Ok(owner_id)
                }
                NativeVerifyResult::Invalid(e) => {
                    Err(Eip8130ValidationError::SenderAuthInvalid(e.to_string()))
                }
                NativeVerifyResult::Unsupported => verify_custom_via_evm(
                    state,
                    verifier,
                    sig_hash,
                    &data,
                    sender,
                    OwnerScope::SENDER,
                    OwnerRole::Sender,
                    custom_verifier_gas_limit,
                ),
            }
        }
    }
}

/// Validates `payer_auth` for a sponsored AA transaction.
///
/// Returns the authenticated payer `owner_id` on success.
fn validate_payer(
    tx: &TxEip8130,
    payer: Address,
    state: &dyn reth_storage_api::StateProvider,
    custom_verifier_gas_limit: u64,
) -> Result<B256, Eip8130ValidationError> {
    if tx.payer_auth.len() < 20 {
        return Err(Eip8130ValidationError::PayerAuthInvalid(
            "payer_auth too short for verifier address".into(),
        ));
    }

    let sig_hash = payer_signature_hash(tx);

    let verifier = Address::from_slice(&tx.payer_auth[..20]);
    let data = Bytes::copy_from_slice(&tx.payer_auth[20..]);

    let result = try_native_verify(verifier, &data, sig_hash);
    match result {
        NativeVerifyResult::Verified(owner_id) => {
            check_owner_authorized(
                state,
                payer,
                owner_id,
                verifier,
                OwnerScope::PAYER,
                OwnerRole::Payer,
            )?;
            Ok(owner_id)
        }
        NativeVerifyResult::Invalid(e) => {
            Err(Eip8130ValidationError::PayerAuthInvalid(e.to_string()))
        }
        NativeVerifyResult::Unsupported => verify_custom_via_evm(
            state,
            verifier,
            sig_hash,
            &data,
            payer,
            OwnerScope::PAYER,
            OwnerRole::Payer,
            custom_verifier_gas_limit,
        ),
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

    if verifier == REVOKED_VERIFIER {
        return Err(role.not_authorized("owner explicitly revoked".into()));
    }

    if verifier != Address::ZERO {
        if verifier != expected_verifier {
            return Err(role.not_authorized(format!(
                "owner_config verifier mismatch: expected {expected_verifier}, got {verifier}"
            )));
        }
        if scope != 0 && (scope & required_scope) == 0 {
            return Err(role
                .not_authorized(format!("owner lacks required scope bit 0x{required_scope:02x}")));
        }
        return Ok(());
    }

    // verifier == address(0): empty slot, implicit EOA rule.
    let implicit_id = implicit_eoa_owner_id(account);
    if owner_id == implicit_id && expected_verifier == K1_VERIFIER_ADDRESS {
        return Ok(());
    }

    Err(role.not_authorized("no owner_config and implicit EOA rule doesn't apply".into()))
}

/// Distinguishes between sender, payer, and authorizer roles for error reporting.
#[derive(Debug, Clone, Copy)]
enum OwnerRole {
    Sender,
    Payer,
    Authorizer,
}

impl OwnerRole {
    fn not_authorized(self, detail: String) -> Eip8130ValidationError {
        match self {
            Self::Sender => Eip8130ValidationError::SenderNotAuthorized(detail),
            Self::Payer => Eip8130ValidationError::PayerNotAuthorized(detail),
            Self::Authorizer => Eip8130ValidationError::AuthorizerNotAuthorized(detail),
        }
    }
}

/// Validates the authorizer chain for config change entries at mempool time.
///
/// For each `ConfigChangeEntry`:
/// 1. Computes the config change digest.
/// 2. Parses `authorizer_auth` and verifies the signature (native or custom).
/// 3. Checks the authenticated owner_id has CONFIG scope in `owner_config`.
/// 4. Tracks pending additions in-memory for chained authorization.
///
/// Also validates authorizer custom verifiers against the allowlist and
/// rejects verifiers with EIP-7702 delegation bytecode.
fn validate_authorizer_chain(
    tx: &TxEip8130,
    sender: Address,
    state: &dyn reth_storage_api::StateProvider,
    verifier_allowlist: Option<&VerifierAllowlist>,
    custom_verifier_gas_limit: u64,
) -> Result<(), Eip8130ValidationError> {
    use std::collections::HashMap;

    let mut pending_owners: HashMap<B256, (Address, u8)> = HashMap::new();

    for entry in &tx.account_changes {
        let cc = match entry {
            AccountChangeEntry::ConfigChange(cc) => cc,
            _ => continue,
        };

        if cc.authorizer_auth.len() < 20 {
            return Err(Eip8130ValidationError::AuthorizerAuthInvalid(
                "authorizer_auth too short for verifier address".into(),
            ));
        }

        let digest = config_change_digest(sender, cc);
        let auth = &cc.authorizer_auth;
        let verifier = Address::from_slice(&auth[..20]);
        let data = Bytes::copy_from_slice(&auth[20..]);

        // Allowlist check for authorizer verifiers.
        if let Some(allowlist) = verifier_allowlist {
            if !allowlist.is_allowed(&verifier) {
                return Err(Eip8130ValidationError::VerifierNotAllowed(verifier));
            }
        }

        // EIP-7702 delegation check for custom authorizer verifiers.
        if !is_native_verifier(verifier) {
            if let Ok(Some(code)) = state.account_code(&verifier) {
                if code.original_bytes().starts_with(&[0xef, 0x01, 0x00]) {
                    return Err(Eip8130ValidationError::VerifierEip7702Delegated(verifier));
                }
            }
        }

        let result = try_native_verify(verifier, &data, digest);
        let owner_id = match result {
            NativeVerifyResult::Verified(id) => {
                check_owner_authorized_with_pending(
                    state,
                    sender,
                    id,
                    verifier,
                    OwnerScope::CONFIG,
                    &pending_owners,
                )?;
                id
            }
            NativeVerifyResult::Invalid(e) => {
                return Err(Eip8130ValidationError::AuthorizerAuthInvalid(e.to_string()));
            }
            NativeVerifyResult::Unsupported => verify_custom_via_evm(
                state,
                verifier,
                digest,
                &data,
                sender,
                OwnerScope::CONFIG,
                OwnerRole::Authorizer,
                custom_verifier_gas_limit,
            )?,
        };

        // Track pending additions/revocations for chaining.
        for op in &cc.operations {
            if op.op_type == 0x01 {
                pending_owners.insert(op.owner_id, (op.verifier, op.scope));
            } else if op.op_type == 0x02 {
                pending_owners.remove(&op.owner_id);
            }
        }

        let _ = owner_id; // used for scope check above
    }

    Ok(())
}

/// Like [`check_owner_authorized`] but also checks pending additions from
/// earlier config change entries in the chain. Pending owners take priority
/// over on-chain state, enabling chained authorization within a single tx.
fn check_owner_authorized_with_pending(
    state: &dyn reth_storage_api::StateProvider,
    account: Address,
    owner_id: B256,
    expected_verifier: Address,
    required_scope: u8,
    pending_owners: &std::collections::HashMap<B256, (Address, u8)>,
) -> Result<(), Eip8130ValidationError> {
    if let Some((verifier, scope)) = pending_owners.get(&owner_id) {
        if *verifier != expected_verifier {
            return Err(Eip8130ValidationError::AuthorizerNotAuthorized(format!(
                "pending owner verifier mismatch: expected {expected_verifier}, got {verifier}"
            )));
        }
        if *scope != 0 && (*scope & required_scope) == 0 {
            return Err(Eip8130ValidationError::AuthorizerNotAuthorized(format!(
                "pending owner lacks required scope 0x{required_scope:02x}"
            )));
        }
        return Ok(());
    }

    check_owner_authorized(
        state,
        account,
        owner_id,
        expected_verifier,
        required_scope,
        OwnerRole::Authorizer,
    )
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
    trusted_payer_bytecodes: &HashSet<B256>,
) -> Result<Eip8130ValidationOutcome, Eip8130ValidationError>
where
    Tx: OpPooledTx + Transaction,
    Client: StateProviderFactory,
{
    let encoded_len = transaction.encoded_2718().len();
    if encoded_len > MAX_AA_TX_ENCODED_BYTES {
        return Err(Eip8130ValidationError::TxTooLarge {
            size: encoded_len,
            limit: MAX_AA_TX_ENCODED_BYTES,
        });
    }

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

    // 2b. Verifier allowlist — reject verifiers not on the list.
    //     Native verifier addresses are always included in the allowlist.
    if let Some(allowlist) = verifier_allowlist {
        if !tx.is_eoa() && tx.sender_auth.len() >= 20 {
            let addr = Address::from_slice(&tx.sender_auth[..20]);
            if !allowlist.is_allowed(&addr) {
                return Err(Eip8130ValidationError::VerifierNotAllowed(addr));
            }
        }
        if tx.payer != Address::ZERO && tx.payer != tx.effective_sender() && tx.payer_auth.len() >= 20 {
            let addr = Address::from_slice(&tx.payer_auth[..20]);
            if !allowlist.is_allowed(&addr) {
                return Err(Eip8130ValidationError::VerifierNotAllowed(addr));
            }
        }
    }

    // 3. Resolve the sender address. For EOA mode (`from == Address::ZERO`),
    //    ecrecover derives the real sender. This must happen before any state
    //    reads that key on the sender address (nonce, lock, sequence, balance).
    let (sender, eoa_owner_id) = resolve_sender_address(&tx)?;

    // 4. Open state provider for storage reads
    let state = client.latest().map_err(|e| Eip8130ValidationError::StateError(e.to_string()))?;

    // 4b. Reject custom verifiers whose bytecode starts with the EIP-7702
    //     delegation designator (0xef0100). Delegated accounts must not be
    //     used as verifier contracts.
    for auth_blob in [&tx.sender_auth, &tx.payer_auth] {
        if auth_blob.len() >= 20 {
            let addr = Address::from_slice(&auth_blob[..20]);
            if !is_native_verifier(addr) {
                if let Ok(Some(code)) = state.account_code(&addr) {
                    if code.original_bytes().starts_with(&[0xef, 0x01, 0x00]) {
                        return Err(Eip8130ValidationError::VerifierEip7702Delegated(addr));
                    }
                }
            }
        }
    }

    // 5. Nonce validation (skipped in nonce-free mode)
    let current_nonce = if tx.nonce_key != NONCE_KEY_MAX {
        let nonce_key_slot = nonce_slot(sender, tx.nonce_key);
        let current = read_storage(&*state, NONCE_MANAGER_ADDRESS, nonce_key_slot)?.to::<u64>();
        if current != tx.nonce_sequence {
            return Err(Eip8130ValidationError::NonceMismatch {
                expected: current,
                got: tx.nonce_sequence,
            });
        }
        current
    } else {
        if tx.expiry > block_timestamp + NONCE_FREE_MAX_EXPIRY_WINDOW {
            return Err(Eip8130ValidationError::NonceFreeExpiryTooFar {
                expiry: tx.expiry,
                max_allowed: block_timestamp + NONCE_FREE_MAX_EXPIRY_WINDOW,
            });
        }
        // Pre-check the on-chain expiring-nonce seen set for replay
        let sig_hash = sender_signature_hash(&tx);
        let seen_slot = expiring_seen_slot(sig_hash);
        let seen_expiry =
            read_storage(&*state, NONCE_MANAGER_ADDRESS, seen_slot)?.to::<u64>();
        if seen_expiry != 0 && seen_expiry > block_timestamp {
            return Err(Eip8130ValidationError::NonceFreeReplay);
        }
        0
    };

    // 6. Lock state — reject config changes on locked accounts
    let has_config_changes =
        tx.account_changes.iter().any(|e| matches!(e, AccountChangeEntry::ConfigChange(_)));
    if has_config_changes {
        if !base_alloy_consensus::is_account_config_known_deployed() {
            let deployed = state
                .account_code(&ACCOUNT_CONFIG_ADDRESS)
                .map_err(|e| Eip8130ValidationError::StateError(e.to_string()))?
                .is_some_and(|code| !code.is_empty());
            if deployed {
                base_alloy_consensus::mark_account_config_deployed();
            } else {
                return Err(Eip8130ValidationError::AccountConfigNotDeployed);
            }
        }
        let lock_slot_key = lock_slot(sender);
        let lock_value = read_storage(&*state, ACCOUNT_CONFIG_ADDRESS, lock_slot_key)?;
        let lock_bytes = lock_value.to_be_bytes::<32>();
        let mut ua = [0u8; 8];
        ua[3..8].copy_from_slice(&lock_bytes[11..16]);
        let unlocks_at = u64::from_be_bytes(ua);
        if block_timestamp < unlocks_at {
            return Err(Eip8130ValidationError::AccountLocked);
        }
    }

    // 7. Config change validation: operation count limit, sequence check,
    //    and authorizer chain verification.
    let total_config_ops: usize = tx
        .account_changes
        .iter()
        .filter_map(|e| match e {
            AccountChangeEntry::ConfigChange(cc) => Some(cc.operations.len()),
            _ => None,
        })
        .sum();
    if total_config_ops > MAX_CONFIG_OPS_PER_TX {
        return Err(Eip8130ValidationError::TooManyConfigOperations {
            count: total_config_ops,
            limit: MAX_CONFIG_OPS_PER_TX,
        });
    }

    if has_config_changes {
        let seq_slot = sequence_base_slot(sender);
        let packed = read_storage(&*state, ACCOUNT_CONFIG_ADDRESS, seq_slot)?;
        let mut expected_multichain = read_sequence(packed, true);
        let mut expected_local = read_sequence(packed, false);

        for entry in &tx.account_changes {
            if let AccountChangeEntry::ConfigChange(change) = entry {
                if change.chain_id == 0 {
                    if change.sequence != expected_multichain {
                        return Err(Eip8130ValidationError::SequenceMismatch {
                            expected: expected_multichain,
                            got: change.sequence,
                        });
                    }
                    expected_multichain = expected_multichain.saturating_add(1);
                } else {
                    if change.sequence != expected_local {
                        return Err(Eip8130ValidationError::SequenceMismatch {
                            expected: expected_local,
                            got: change.sequence,
                        });
                    }
                    expected_local = expected_local.saturating_add(1);
                }
            }
        }
    }

    // 7b. Authorizer chain validation for config changes.
    if has_config_changes {
        validate_authorizer_chain(
            &tx,
            sender,
            &*state,
            verifier_allowlist,
            custom_verifier_gas_limit,
        )?;
    }

    // 8. Compute intrinsic gas for the balance check. `gas_limit` is the
    //    sender's execution-only budget, so we don't compare it against
    //    intrinsic gas. Nonce key is "warm" if the current sequence > 0.
    let nonce_key_is_warm = current_nonce > 0;
    let aa_intrinsic_gas = intrinsic_gas(&tx, nonce_key_is_warm, tx.chain_id);

    // 9. Sender authorization (checks owner_config for the resolved sender)
    let sender_owner_id = validate_sender_authorization(
        &tx,
        sender,
        eoa_owner_id,
        &*state,
        custom_verifier_gas_limit,
    )?;

    // 10. Payer resolution and authorization
    let payer = if tx.is_self_pay() { sender } else { tx.payer };
    let payer_owner_id = if payer != sender {
        Some(validate_payer(&tx, payer, &*state, custom_verifier_gas_limit)?)
    } else {
        None
    };

    // 11. Balance check — payer must cover max gas cost.
    //     Total = (intrinsic + custom_verifier_cap + execution_gas_limit) * max_fee_per_gas
    let sender_is_custom = !tx.is_eoa()
        && tx.sender_auth.len() >= 20
        && !is_native_verifier(Address::from_slice(&tx.sender_auth[..20]));
    let payer_is_custom = !tx.is_self_pay()
        && tx.payer_auth.len() >= 20
        && !is_native_verifier(Address::from_slice(&tx.payer_auth[..20]));
    let authorizer_is_custom = tx.account_changes.iter().any(|e| {
        matches!(e, AccountChangeEntry::ConfigChange(cc)
            if cc.authorizer_auth.len() >= 20
                && !is_native_verifier(Address::from_slice(&cc.authorizer_auth[..20])))
    });
    let has_custom_verifier = sender_is_custom || payer_is_custom || authorizer_is_custom;
    let verifier_gas_cap = if has_custom_verifier { CUSTOM_VERIFIER_GAS_CAP } else { 0u64 };
    let total_gas =
        U256::from(aa_intrinsic_gas) + U256::from(verifier_gas_cap) + U256::from(tx.gas_limit);
    let max_gas_cost = total_gas.saturating_mul(U256::from(tx.max_fee_per_gas));
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
        sender,
        Some(sender_owner_id).filter(|id| *id != B256::ZERO),
        payer_owner_id.filter(|id| *id != B256::ZERO),
    );

    let sponsored_payer = if payer != sender { Some(payer) } else { None };

    // 13. Determine sender throughput tier for the 2D nonce pool.
    //     Read the lock state only when we haven't already (step 6 reads it
    //     only when there are config changes).
    let sender_throughput_tier =
        compute_sender_throughput_tier(sender, payer, &*state, trusted_payer_bytecodes);

    Ok(Eip8130ValidationOutcome {
        balance,
        state_nonce: tx.nonce_sequence,
        nonce_key: tx.nonce_key,
        sender_owner_id,
        invalidation_keys,
        sponsored_payer,
        sender_throughput_tier,
    })
}

/// Determines the [`SenderThroughputTier`] by reading the sender's lock state
/// and, when the sender is locked, checking whether the payer has trusted
/// bytecode.
///
/// Returns [`SenderThroughputTier::Default`] on any state-read error so that
/// failures degrade gracefully to the standard limit.
fn compute_sender_throughput_tier(
    sender: Address,
    payer: Address,
    state: &dyn reth_storage_api::StateProvider,
    trusted_payer_bytecodes: &HashSet<B256>,
) -> SenderThroughputTier {
    let lock_slot_key = lock_slot(sender);
    let lock_value = match read_storage(state, ACCOUNT_CONFIG_ADDRESS, lock_slot_key) {
        Ok(v) => v,
        Err(_) => return SenderThroughputTier::Default,
    };

    let lock_bytes = lock_value.to_be_bytes::<32>();
    let mut ua = [0u8; 8];
    ua[3..8].copy_from_slice(&lock_bytes[11..16]);
    let unlocks_at = u64::from_be_bytes(ua);
    // We don't have the exact block timestamp here, so use the current
    // `unlocksAt != 0` as a heuristic. A zero value means no lock was
    // ever set; any non-zero value means the account was locked (or is
    // in the process of unlocking). This over-classifies slightly for
    // accounts that have fully unlocked but haven't cleared storage yet,
    // which is safe — it only upgrades the throughput tier.
    if unlocks_at == 0 {
        return SenderThroughputTier::Default;
    }

    if trusted_payer_bytecodes.is_empty() {
        return SenderThroughputTier::Locked;
    }

    // For the highest tier, check the payer's bytecode hash against the
    // trusted set. In the self-pay case the payer IS the sender.
    let payer_code_hash = match state.account_code(&payer) {
        Ok(Some(code)) => {
            use alloy_primitives::keccak256;
            keccak256(code.original_bytes())
        }
        _ => return SenderThroughputTier::Locked,
    };

    if trusted_payer_bytecodes.contains(&payer_code_hash) {
        SenderThroughputTier::LockedTrustedPayer
    } else {
        SenderThroughputTier::Locked
    }
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
}
