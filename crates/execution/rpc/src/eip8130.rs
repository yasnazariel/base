//! EIP-8130 Account Abstraction RPC extensions.
//!
//! Provides the `base_getEip8130Nonce` method for querying 2D nonces and will
//! eventually host other EIP-8130-specific RPC methods.

use alloy_primitives::{Address, U256};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

/// RPC interface for EIP-8130 Account Abstraction queries.
#[rpc(server, namespace = "base")]
pub trait Eip8130Api {
    /// Returns the current nonce for an EIP-8130 account's 2D nonce key.
    ///
    /// The nonce key is a `uint192` that identifies a specific nonce lane.
    /// Standard EOA nonce corresponds to nonce_key = 0.
    #[method(name = "getEip8130Nonce")]
    async fn get_eip8130_nonce(
        &self,
        address: Address,
        nonce_key: U256,
    ) -> RpcResult<u64>;
}
