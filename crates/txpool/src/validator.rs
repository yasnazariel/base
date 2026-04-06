//! Optimism-aware transaction validator wrapping Reth's [`EthTransactionValidator`].
//!
//! [`OpTransactionValidator`] validates incoming transactions against both
//! standard Ethereum rules and Optimism / EIP-8130 specific constraints,
//! routing 2D-nonce AA transactions into the [`Eip8130Pool`] side-pool.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use alloy_consensus::{BlockHeader, Transaction};
use alloy_primitives::{Address, B256};
use base_alloy_chains::BaseUpgrades;
use base_execution_evm::RethL1BlockInfo;
use base_revm::L1BlockInfo;
use parking_lot::RwLock;
use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_evm::ConfigureEvm;
use reth_primitives_traits::{
    Block, BlockBody, BlockTy, GotExpected, SealedBlock,
    transaction::error::InvalidTransactionError,
};
use reth_storage_api::{AccountInfoReader, BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    EthPoolTransaction, EthTransactionValidator, TransactionOrigin, TransactionValidationOutcome,
    TransactionValidator, validate::ValidTransaction,
};

use base_alloy_consensus::{AA_TX_TYPE_ID, nonce_slot};

use crate::{
    Eip8130InvalidationIndex, Eip8130Pool, Eip8130PoolConfig, Eip8130TxId, OpPooledTx,
    SharedEip8130Pool, VerifierAllowlist, is_2d_nonce,
};

/// Tracks additional infos for the current block.
#[derive(Debug, Default)]
pub struct OpL1BlockInfo {
    /// The current L1 block info.
    l1_block_info: RwLock<L1BlockInfo>,
    /// Current block timestamp.
    timestamp: AtomicU64,
}

impl OpL1BlockInfo {
    /// Returns the most recent timestamp
    pub fn timestamp(&self) -> u64 {
        self.timestamp.load(Ordering::Relaxed)
    }
}

/// Validator for Base transactions.
#[derive(Debug, Clone)]
pub struct OpTransactionValidator<Client, Tx, Evm> {
    /// The type that performs the actual validation.
    inner: Arc<EthTransactionValidator<Client, Tx, Evm>>,
    /// Additional block info required for validation.
    block_info: Arc<OpL1BlockInfo>,
    /// If true, ensure that the transaction's sender has enough balance to cover the L1 gas fee
    /// derived from the tracked L1 block info that is extracted from the first transaction in the
    /// L2 block.
    require_l1_data_gas_fee: bool,
    /// EIP-8130 verifier allowlist. `None` means all verifiers are accepted.
    /// When set, only native verifiers and the configured custom verifier
    /// addresses are accepted into the mempool.
    verifier_allowlist: Option<Arc<VerifierAllowlist>>,
    /// Gas limit for custom verifier STATICCALL in the txpool EVM.
    custom_verifier_gas_limit: u64,
    /// Shared index of storage-slot dependencies for pending AA transactions,
    /// used by the invalidation maintenance task to evict transactions whose
    /// underlying state has changed.
    invalidation_index: Arc<RwLock<Eip8130InvalidationIndex>>,
    /// 2D nonce pool for AA transactions with `nonce_key != 0`.
    /// These transactions bypass the standard Reth pool (which would collide
    /// on `(sender, nonce_sequence)`) and are stored here instead.
    eip8130_pool: SharedEip8130Pool<Tx>,
    /// Set of keccak256 bytecode hashes considered "trusted". When an account
    /// (sender or payer) is locked and its deployed bytecode matches one of
    /// these hashes, it qualifies for the elevated throughput tier in the 2D
    /// nonce pool.
    trusted_payer_bytecodes: HashSet<B256>,
}

impl<Client, Tx, Evm> OpTransactionValidator<Client, Tx, Evm> {
    /// Returns the configured chain spec
    pub fn chain_spec(&self) -> Arc<Client::ChainSpec>
    where
        Client: ChainSpecProvider,
    {
        self.inner.chain_spec()
    }

    /// Returns the configured client
    pub fn client(&self) -> &Client {
        self.inner.client()
    }

    /// Returns the current block timestamp.
    fn block_timestamp(&self) -> u64 {
        self.block_info.timestamp.load(Ordering::Relaxed)
    }

    /// Whether to ensure that the transaction's sender has enough balance to also cover the L1 gas
    /// fee.
    pub fn require_l1_data_gas_fee(self, require_l1_data_gas_fee: bool) -> Self {
        Self { require_l1_data_gas_fee, ..self }
    }

    /// Returns whether this validator also requires the transaction's sender to have enough balance
    /// to cover the L1 gas fee.
    pub const fn requires_l1_data_gas_fee(&self) -> bool {
        self.require_l1_data_gas_fee
    }

    /// Sets the EIP-8130 verifier allowlist.
    pub fn with_verifier_allowlist(self, allowlist: VerifierAllowlist) -> Self {
        Self { verifier_allowlist: Some(Arc::new(allowlist)), ..self }
    }

    /// Sets the trusted bytecode hashes for elevated throughput tiers.
    ///
    /// When an account is locked and its deployed bytecode hash matches one
    /// of these, it qualifies for the elevated throughput tier in the 2D
    /// nonce pool.
    pub fn with_trusted_payer_bytecodes(self, hashes: HashSet<B256>) -> Self {
        Self { trusted_payer_bytecodes: hashes, ..self }
    }

    /// Sets the gas limit for custom verifier STATICCALL in the txpool EVM.
    pub fn with_custom_verifier_gas_limit(self, gas_limit: u64) -> Self {
        Self { custom_verifier_gas_limit: gas_limit, ..self }
    }

    /// Overrides the EIP-8130 pool configuration (throughput limits, TTL, etc).
    pub fn with_eip8130_pool_config(self, config: Eip8130PoolConfig) -> Self {
        Self { eip8130_pool: Arc::new(Eip8130Pool::with_config(config)), ..self }
    }
}

impl<Client, Tx, Evm> OpTransactionValidator<Client, Tx, Evm>
where
    Client:
        ChainSpecProvider<ChainSpec: BaseUpgrades> + StateProviderFactory + BlockReaderIdExt + Sync,
    Tx: EthPoolTransaction + OpPooledTx + Clone,
    Evm: ConfigureEvm,
{
    /// Create a new [`OpTransactionValidator`].
    pub fn new(inner: EthTransactionValidator<Client, Tx, Evm>) -> Self {
        let this = Self::with_block_info(inner, OpL1BlockInfo::default());
        if let Ok(Some(block)) =
            this.inner.client().block_by_number_or_tag(alloy_eips::BlockNumberOrTag::Latest)
        {
            // genesis block has no txs, so we can't extract L1 info, we set the block info to empty
            // so that we will accept txs into the pool before the first block
            if block.header().number() == 0 {
                this.block_info.timestamp.store(block.header().timestamp(), Ordering::Relaxed);
            } else {
                this.update_l1_block_info(block.header(), block.body().transactions().first());
            }
        }

        this
    }

    /// Create a new [`OpTransactionValidator`] with the given [`OpL1BlockInfo`].
    pub fn with_block_info(
        inner: EthTransactionValidator<Client, Tx, Evm>,
        block_info: OpL1BlockInfo,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            block_info: Arc::new(block_info),
            require_l1_data_gas_fee: true,
            verifier_allowlist: None,
            custom_verifier_gas_limit: crate::DEFAULT_CUSTOM_VERIFIER_GAS_LIMIT,
            invalidation_index: Arc::new(RwLock::new(Eip8130InvalidationIndex::default())),
            eip8130_pool: Arc::new(Eip8130Pool::new()),
            trusted_payer_bytecodes: HashSet::new(),
        }
    }

    /// Returns a shared reference to the EIP-8130 invalidation index.
    ///
    /// Pass this to [`maintain_eip8130_invalidation`] so the maintenance task
    /// can read the same index that the validator populates.
    pub fn invalidation_index(&self) -> Arc<RwLock<Eip8130InvalidationIndex>> {
        Arc::clone(&self.invalidation_index)
    }

    /// Returns a shared reference to the EIP-8130 2D nonce pool.
    ///
    /// AA transactions with `nonce_key != 0` are stored here instead of the
    /// standard Reth pool to avoid `(sender, nonce_sequence)` collisions.
    pub fn eip8130_pool(&self) -> SharedEip8130Pool<Tx> {
        Arc::clone(&self.eip8130_pool)
    }

    /// Update the L1 block info for the given header and system transaction, if any.
    ///
    /// Note: this supports optional system transaction, in case this is used in a dev setup
    pub fn update_l1_block_info<H, T>(&self, header: &H, tx: Option<&T>)
    where
        H: BlockHeader,
        T: Transaction,
    {
        self.block_info.timestamp.store(header.timestamp(), Ordering::Relaxed);

        if let Some(Ok(l1_block_info)) = tx.map(base_execution_evm::extract_l1_info_from_tx) {
            *self.block_info.l1_block_info.write() = l1_block_info;
        }
    }

    /// Validates a single transaction.
    ///
    /// See also [`TransactionValidator::validate_transaction`]
    ///
    /// This behaves the same as [`OpTransactionValidator::validate_one_with_state`], but creates
    /// a new state provider internally.
    pub async fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        self.validate_one_with_state(origin, transaction, &mut None).await
    }

    /// Validates a single transaction with a provided state provider.
    ///
    /// This allows reusing the same state provider across multiple transaction validations.
    ///
    /// See also [`TransactionValidator::validate_transaction`]
    ///
    /// This behaves the same as [`EthTransactionValidator::validate_one_with_state`], but in
    /// addition applies OP validity checks:
    /// - ensures tx is not eip4844
    /// - ensures that the account has enough balance to cover the L1 gas cost
    pub async fn validate_one_with_state(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
        state: &mut Option<Box<dyn AccountInfoReader + Send>>,
    ) -> TransactionValidationOutcome<Tx> {
        if transaction.is_eip4844() {
            return TransactionValidationOutcome::Invalid(
                transaction,
                InvalidTransactionError::TxTypeNotSupported.into(),
            );
        }

        if transaction.ty() == AA_TX_TYPE_ID {
            if !self.chain_spec().is_base_v1_active_at_timestamp(self.block_timestamp()) {
                return TransactionValidationOutcome::Invalid(
                    transaction,
                    InvalidTransactionError::TxTypeNotSupported.into(),
                );
            }

            return match crate::validate_eip8130_transaction(
                &transaction,
                self.block_timestamp(),
                self.chain_spec().chain().id(),
                self.client(),
                self.verifier_allowlist.as_deref(),
                self.custom_verifier_gas_limit,
                &self.trusted_payer_bytecodes,
            ) {
                Ok(outcome) => {
                    if self.requires_l1_data_gas_fee() {
                        let mut l1_info = self.block_info.l1_block_info.read().clone();
                        let encoded = transaction.encoded_2718();
                        match l1_info.l1_tx_data_fee(
                            self.chain_spec(),
                            self.block_timestamp(),
                            &encoded,
                            false,
                        ) {
                            Ok(l1_cost) => {
                                let total = transaction.cost().saturating_add(l1_cost);
                                if total > outcome.balance {
                                    return TransactionValidationOutcome::Invalid(
                                        transaction,
                                        InvalidTransactionError::InsufficientFunds(
                                            GotExpected { got: outcome.balance, expected: total }
                                                .into(),
                                        )
                                        .into(),
                                    );
                                }
                            }
                            Err(err) => {
                                return TransactionValidationOutcome::Error(
                                    *transaction.hash(),
                                    Box::new(err),
                                );
                            }
                        }
                    }

                    // Route AA txs with nonce_key != 0 to the 2D nonce pool to
                    // avoid collisions in the standard pool's (sender, nonce) key.
                    let nonce_key = outcome.nonce_key;
                    if is_2d_nonce(nonce_key) {
                        let sender = transaction.sender();
                        let payer = outcome.sponsored_payer.unwrap_or(sender);
                        let nonce_storage_slot = nonce_slot(sender, nonce_key);
                        let id =
                            Eip8130TxId { sender, nonce_key, nonce_sequence: outcome.state_nonce };
                        let client = self.client();
                        let trusted = &self.trusted_payer_bytecodes;
                        let block_ts = self.block_timestamp();
                        let check_tier = |account: Address| -> crate::TierCheckResult {
                            let state = match client.latest() {
                                Ok(s) => s,
                                Err(_) => {
                                    return crate::TierCheckResult {
                                        tier: crate::ThroughputTier::Default,
                                        cache_for: None,
                                    };
                                }
                            };
                            crate::compute_account_tier(account, &*state, trusted, block_ts)
                        };
                        if let Err(err) = self.eip8130_pool.add_transaction(
                            id,
                            transaction.clone(),
                            payer,
                            origin,
                            nonce_storage_slot,
                            &check_tier,
                        ) {
                            tracing::debug!(
                                target: "txpool",
                                error = %err,
                                "EIP-8130 2D pool rejected transaction"
                            );
                            return TransactionValidationOutcome::Invalid(
                                transaction,
                                reth_transaction_pool::error::InvalidPoolTransactionError::other(
                                    err,
                                ),
                            );
                        }
                    }

                    if !outcome.invalidation_keys.is_empty() {
                        let tx_hash = *transaction.hash();
                        self.invalidation_index.write().insert(
                            tx_hash,
                            outcome.invalidation_keys.clone(),
                            outcome.sponsored_payer,
                        );
                    }

                    TransactionValidationOutcome::Valid {
                        balance: outcome.balance,
                        state_nonce: outcome.state_nonce,
                        transaction: ValidTransaction::Valid(transaction),
                        propagate: true,
                        bytecode_hash: None,
                        authorities: None,
                    }
                }
                Err(e) => {
                    tracing::debug!(target: "txpool", error = %e, "EIP-8130 transaction validation failed");
                    TransactionValidationOutcome::Invalid(
                        transaction,
                        reth_transaction_pool::error::InvalidPoolTransactionError::other(e),
                    )
                }
            };
        }

        let outcome = self.inner.validate_one_with_state(origin, transaction, state);

        self.apply_op_checks(outcome)
    }

    /// Performs the necessary opstack specific checks based on top of the regular eth outcome.
    fn apply_op_checks(
        &self,
        outcome: TransactionValidationOutcome<Tx>,
    ) -> TransactionValidationOutcome<Tx> {
        if !self.requires_l1_data_gas_fee() {
            // no need to check L1 gas fee
            return outcome;
        }
        // ensure that the account has enough balance to cover the L1 gas cost
        if let TransactionValidationOutcome::Valid {
            balance,
            state_nonce,
            transaction: valid_tx,
            propagate,
            bytecode_hash,
            authorities,
        } = outcome
        {
            let mut l1_block_info = self.block_info.l1_block_info.read().clone();

            let encoded = valid_tx.transaction().encoded_2718();

            let cost_addition = match l1_block_info.l1_tx_data_fee(
                self.chain_spec(),
                self.block_timestamp(),
                &encoded,
                false,
            ) {
                Ok(cost) => cost,
                Err(err) => {
                    return TransactionValidationOutcome::Error(*valid_tx.hash(), Box::new(err));
                }
            };
            let cost = valid_tx.transaction().cost().saturating_add(cost_addition);

            // Checks for max cost
            if cost > balance {
                return TransactionValidationOutcome::Invalid(
                    valid_tx.into_transaction(),
                    InvalidTransactionError::InsufficientFunds(
                        GotExpected { got: balance, expected: cost }.into(),
                    )
                    .into(),
                );
            }

            return TransactionValidationOutcome::Valid {
                balance,
                state_nonce,
                transaction: valid_tx,
                propagate,
                bytecode_hash,
                authorities,
            };
        }
        outcome
    }
}

impl<Client, Tx, Evm> TransactionValidator for OpTransactionValidator<Client, Tx, Evm>
where
    Client:
        ChainSpecProvider<ChainSpec: BaseUpgrades> + StateProviderFactory + BlockReaderIdExt + Sync,
    Tx: EthPoolTransaction + OpPooledTx + Clone,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;
    type Block = BlockTy<Evm::Primitives>;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        self.validate_one(origin, transaction).await
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock<Self::Block>) {
        self.inner.on_new_head_block(new_tip_block);
        self.update_l1_block_info(
            new_tip_block.header(),
            new_tip_block.body().transactions().first(),
        );
    }
}
