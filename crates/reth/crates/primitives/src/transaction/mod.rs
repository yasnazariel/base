//! Transaction types.

pub use alloy_consensus::transaction::PooledTransaction;
use once_cell as _;
pub use reth_primitives_traits::{
    FillTxEnv, WithEncoded,
    sync::{LazyLock, OnceLock},
    transaction::{
        error::{
            InvalidTransactionError, TransactionConversionError, TryFromRecoveredTransactionError,
        },
        signed::SignedTransaction,
    },
};
pub use signature::{recover_signer, recover_signer_unchecked};
pub use tx_type::TxType;

/// Handling transaction signature operations, including signature recovery,
/// applying chain IDs, and EIP-2 validation.
pub mod signature;
pub mod util;

mod tx_type;

/// Base transaction types.
pub use base_alloy_consensus::{
    OpPooledTransaction, OpTransaction, OpTxEnvelope as OpTransactionSigned, OpTxType,
    OpTypedTransaction,
};
/// Signed transaction.
pub use reth_ethereum_primitives::{Transaction, TransactionSigned};
