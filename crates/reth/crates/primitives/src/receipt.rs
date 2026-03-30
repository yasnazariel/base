/// Base receipt type.
pub use base_alloy_consensus::OpReceipt;
/// Receipt containing result of transaction execution.
pub use reth_ethereum_primitives::Receipt;
/// Retrieves gas spent by transactions as a vector of tuples (transaction index, gas used).
pub use reth_primitives_traits::receipt::gas_spent_by_transactions;

/// Trait for Base deposit receipts.
pub trait DepositReceipt: reth_primitives_traits::Receipt {
    /// Converts a receipt into a mutable Base deposit receipt.
    fn as_deposit_receipt_mut(&mut self) -> Option<&mut base_alloy_consensus::OpDepositReceipt>;

    /// Extracts a Base deposit receipt from a receipt.
    fn as_deposit_receipt(&self) -> Option<&base_alloy_consensus::OpDepositReceipt>;
}

impl DepositReceipt for OpReceipt {
    fn as_deposit_receipt_mut(&mut self) -> Option<&mut base_alloy_consensus::OpDepositReceipt> {
        match self {
            Self::Deposit(receipt) => Some(receipt),
            _ => None,
        }
    }

    fn as_deposit_receipt(&self) -> Option<&base_alloy_consensus::OpDepositReceipt> {
        match self {
            Self::Deposit(receipt) => Some(receipt),
            _ => None,
        }
    }
}
