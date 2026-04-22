//! Shared runtime state for the vibenet faucet HTTP service.

use std::net::IpAddr;
use std::sync::Arc;

use alloy_network::{Ethereum, EthereumWallet};
use alloy_primitives::Address;
use alloy_provider::{
    Identity, ProviderBuilder, RootProvider,
    fillers::{
        BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller,
    },
};
use eyre::Result;

use crate::config::FaucetConfig;
use crate::limiter::Limiter;

/// Concrete provider type used by the faucet. Mirrors the stack installed by
/// [`ProviderBuilder::new`] (recommended fillers) plus our wallet filler so we
/// can sign and submit transactions with a single `.send_transaction` call.
pub(crate) type FaucetProvider = FillProvider<
    JoinFill<
        JoinFill<
            Identity,
            JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider<Ethereum>,
    Ethereum,
>;

/// Which asset a drip request is for. Used as a namespace on the shared
/// cooldown trackers so that a successful ETH drip does not put the caller
/// into cooldown for USDV (and vice versa). Adding a new faucet-dispensed
/// asset only requires adding a variant here.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub(crate) enum Asset {
    Eth,
    Usdv,
}

/// All state shared between HTTP handlers. Cheap to clone because every field
/// is `Arc`-wrapped or trivially copyable.
#[derive(Clone)]
pub struct FaucetState {
    /// Parsed configuration.
    pub(crate) config: Arc<FaucetConfig>,
    /// Wallet-enabled JSON-RPC provider for the upstream L2.
    pub(crate) provider: Arc<FaucetProvider>,
    /// Per-client-IP cooldown tracker, namespaced by asset so ETH and USDV
    /// drips have independent cooldowns.
    pub(crate) ip_limiter: Arc<Limiter<(Asset, IpAddr)>>,
    /// Per-destination-address cooldown tracker, namespaced by asset.
    pub(crate) addr_limiter: Arc<Limiter<(Asset, Address)>>,
}

impl FaucetState {
    /// Build state from a fully-parsed config. Connects to the upstream RPC
    /// over HTTP and installs a wallet filler so handlers do not have to
    /// manage nonces or gas.
    pub fn new(config: FaucetConfig) -> Result<Self> {
        let wallet = EthereumWallet::from(config.signer.clone());
        let rpc_url = config.rpc_url.parse()?;
        let provider = ProviderBuilder::new().wallet(wallet).connect_http(rpc_url);

        Ok(Self {
            config: Arc::new(config),
            provider: Arc::new(provider),
            ip_limiter: Arc::new(Limiter::new()),
            addr_limiter: Arc::new(Limiter::new()),
        })
    }
}

impl std::fmt::Debug for FaucetState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaucetState")
            .field("address", &self.config.address)
            .field("chain_id", &self.config.chain_id)
            .field("drip_wei", &self.config.drip_wei)
            .finish_non_exhaustive()
    }
}
