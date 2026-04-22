//! Environment-driven configuration for the vibenet faucet.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use eyre::{Context, Result, eyre};

/// Fully-parsed faucet configuration. All fields come from environment
/// variables documented in `README.md`.
#[derive(Debug, Clone)]
pub struct FaucetConfig {
    /// Address we bind the HTTP server to.
    pub bind: SocketAddr,
    /// Upstream JSON-RPC URL for the L2.
    pub rpc_url: String,
    /// Chain id of the L2.
    pub chain_id: u64,
    /// Signer for the faucet hot wallet. Never logged.
    pub signer: PrivateKeySigner,
    /// Public address corresponding to `signer`, cached for `/status`.
    pub address: Address,
    /// Amount of wei to drip per successful request.
    pub drip_wei: U256,
    /// Per-client-IP cooldown.
    pub ip_cooldown: Duration,
    /// Per-destination-address cooldown.
    pub addr_cooldown: Duration,
    /// Path to the contracts.json file written by vibenet-setup. Used only
    /// for USDV drips; if the file does not exist or lacks a `usdv` entry,
    /// the USDV drip endpoint returns a helpful error and the ETH drip
    /// still works.
    pub contracts_path: PathBuf,
    /// Number of USDV *base units* (6 decimals) minted per USDV drip. The
    /// default is 1000 * 10^6 = 1000 USDV.
    pub usdv_drip_units: U256,
}

impl FaucetConfig {
    /// Construct from process environment. Returns an error if any required
    /// variable is missing or malformed.
    pub fn from_env() -> Result<Self> {
        let bind: SocketAddr = std::env::var("VIBENET_FAUCET_BIND")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
            .parse()
            .context("VIBENET_FAUCET_BIND is not a valid socket address")?;

        let rpc_url = std::env::var("VIBENET_FAUCET_RPC_URL")
            .context("VIBENET_FAUCET_RPC_URL is required")?;

        let chain_id: u64 = std::env::var("VIBENET_FAUCET_CHAIN_ID")
            .context("VIBENET_FAUCET_CHAIN_ID is required")?
            .parse()
            .context("VIBENET_FAUCET_CHAIN_ID must be an unsigned integer")?;

        let key_hex = std::env::var("VIBENET_FAUCET_PRIVATE_KEY")
            .context("VIBENET_FAUCET_PRIVATE_KEY is required")?;
        let signer: PrivateKeySigner = key_hex
            .trim_start_matches("0x")
            .parse()
            .map_err(|e| eyre!("invalid VIBENET_FAUCET_PRIVATE_KEY: {e}"))?;
        let derived = signer.address();

        if let Ok(declared) = std::env::var("VIBENET_FAUCET_ADDR") {
            let declared = Address::from_str(&declared)
                .context("VIBENET_FAUCET_ADDR is not a valid address")?;
            if declared != derived {
                return Err(eyre!(
                    "VIBENET_FAUCET_ADDR {declared} does not match address derived from \
                     VIBENET_FAUCET_PRIVATE_KEY {derived}"
                ));
            }
        }

        let drip_wei = parse_u256_env("VIBENET_FAUCET_DRIP_WEI", "100000000000000000")?;

        let ip_cooldown =
            Duration::from_secs(parse_u64_env("VIBENET_FAUCET_IP_COOLDOWN_SECS", 3600)?);
        let addr_cooldown =
            Duration::from_secs(parse_u64_env("VIBENET_FAUCET_ADDR_COOLDOWN_SECS", 3600)?);

        let contracts_path = std::env::var("VIBENET_FAUCET_CONTRACTS_PATH")
            .unwrap_or_else(|_| "/shared/contracts.json".to_string())
            .into();
        // 1000 USDV by default. USDV has 6 decimals.
        let usdv_drip_units = parse_u256_env("VIBENET_FAUCET_USDV_DRIP_UNITS", "1000000000")?;

        Ok(Self {
            bind,
            rpc_url,
            chain_id,
            signer,
            address: derived,
            drip_wei,
            ip_cooldown,
            addr_cooldown,
            contracts_path,
            usdv_drip_units,
        })
    }
}

fn parse_u256_env(name: &str, default: &str) -> Result<U256> {
    let raw = std::env::var(name).unwrap_or_else(|_| default.to_string());
    U256::from_str(&raw).map_err(|e| eyre!("{name} is not a valid u256: {e}"))
}

fn parse_u64_env(name: &str, default: u64) -> Result<u64> {
    std::env::var(name).map_or_else(
        |_| Ok(default),
        |s| s.parse().map_err(|e| eyre!("{name} must be unsigned int: {e}")),
    )
}
