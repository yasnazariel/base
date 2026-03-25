//! EIP-8130 Account Abstraction RPC extensions.
//!
//! Provides the `base_getEip8130Nonce` method for querying 2D nonces and will
//! eventually host other EIP-8130-specific RPC methods.

use alloy_primitives::{Address, U256};
use base_alloy_consensus::{NONCE_MANAGER_ADDRESS, nonce_slot};
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use reth_rpc_eth_api::RpcNodeCore;
use reth_storage_api::StateProviderFactory;

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

/// Implements the [`Eip8130ApiServer`] for any type that provides access to
/// a state provider via [`RpcNodeCore`].
#[derive(Debug)]
pub struct Eip8130ApiImpl<Provider> {
    provider: Provider,
}

impl<Provider> Eip8130ApiImpl<Provider> {
    /// Creates a new [`Eip8130ApiImpl`] from a state provider factory.
    pub fn new(provider: Provider) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl<Provider> Eip8130ApiServer for Eip8130ApiImpl<Provider>
where
    Provider: StateProviderFactory + Send + Sync + 'static,
{
    async fn get_eip8130_nonce(
        &self,
        address: Address,
        nonce_key: U256,
    ) -> RpcResult<u64> {
        let slot = nonce_slot(address, nonce_key);
        let state = self.provider.latest().map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                jsonrpsee::types::error::INTERNAL_ERROR_CODE,
                format!("state access error: {e}"),
                None::<()>,
            )
        })?;

        let value = state.storage(NONCE_MANAGER_ADDRESS, slot.into()).map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                jsonrpsee::types::error::INTERNAL_ERROR_CODE,
                format!("storage read error: {e}"),
                None::<()>,
            )
        })?;

        Ok(value.unwrap_or_default().to::<u64>())
    }
}

/// Convenience constructor: builds an [`Eip8130ApiImpl`] from anything that
/// implements [`RpcNodeCore`] (e.g. `OpEthApi`).
pub fn eip8130_api<N: RpcNodeCore>(api: &N) -> Eip8130ApiImpl<N::Provider>
where
    N::Provider: Clone,
{
    Eip8130ApiImpl::new(api.provider().clone())
}
