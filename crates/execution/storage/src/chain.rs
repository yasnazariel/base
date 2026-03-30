use alloy_consensus::Header;
use reth_primitives::OpTransactionSigned;
use reth_storage_api::EmptyBodyStorage;

/// Base storage implementation.
pub type OpStorage<T = OpTransactionSigned, H = Header> = EmptyBodyStorage<T, H>;
