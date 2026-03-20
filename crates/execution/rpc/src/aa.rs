//! EIP-8130 Account Abstraction RPC extensions.
//!
//! Provides the `base_getAaNonce` method for querying 2D nonces and will
//! eventually host other AA-specific RPC methods.

use alloy_primitives::{Address, U256};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

/// RPC interface for EIP-8130 Account Abstraction queries.
#[rpc(server, namespace = "base")]
pub trait AaApi {
    /// Returns the current nonce for an AA account's 2D nonce key.
    ///
    /// The nonce key is a `uint192` that identifies a specific nonce lane.
    /// Standard EOA nonce corresponds to nonce_key = 0.
    #[method(name = "getAaNonce")]
    async fn get_aa_nonce(
        &self,
        address: Address,
        nonce_key: U256,
    ) -> RpcResult<u64>;
}
