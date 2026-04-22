//! Parse environment variables into a typed config.

use eyre::{Result, WrapErr};
use std::{net::SocketAddr, path::PathBuf};

/// All configuration for the explorer. Populated from env vars; see the
/// crate README for the full list.
#[derive(Debug, Clone)]
pub struct ExplorerConfig {
    /// HTTP RPC endpoint of the upstream node. Used for backfill + read-through.
    pub rpc_http_url: String,
    /// WebSocket RPC endpoint. Used for the newHeads subscription so we do
    /// not poll.
    pub rpc_ws_url: String,
    /// Chain ID we expect the upstream node to report. If it doesn't match,
    /// we refuse to start so we don't corrupt a shared DB by indexing the
    /// wrong chain.
    pub expected_chain_id: u64,
    /// Path to the sqlite database file. A fresh file will be created and
    /// migrated on first start.
    pub db_path: PathBuf,
    /// Address the HTTP server binds to.
    pub bind: SocketAddr,
    /// Block to begin backfill from on an empty DB. Useful for shorter
    /// reindexes during development.
    pub start_block: u64,
    /// Number of blocks to fetch in parallel during backfill.
    pub backfill_concurrency: usize,
    /// Base URL the UI surfaces as the RPC endpoint (e.g.
    /// `https://vibenet-rpc.base.org/rpc/<key>`). Optional; only used for
    /// the "connect your wallet" hints on the home page.
    pub public_rpc_url: Option<String>,
    /// Git branch / commit strings for the footer.
    pub branch: String,
    pub commit: String,
}

impl ExplorerConfig {
    /// Read config from the `VIBESCAN_*` env-var namespace. Every field
    /// without a default is required.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            rpc_http_url: require("VIBESCAN_RPC_HTTP_URL")?,
            rpc_ws_url: require("VIBESCAN_RPC_WS_URL")?,
            expected_chain_id: require_parsed("VIBESCAN_CHAIN_ID")?,
            db_path: require("VIBESCAN_DB_PATH")?.into(),
            bind: require_parsed("VIBESCAN_BIND")?,
            start_block: optional_parsed("VIBESCAN_START_BLOCK")?.unwrap_or(0),
            backfill_concurrency: optional_parsed("VIBESCAN_BACKFILL_CONCURRENCY")?.unwrap_or(16),
            public_rpc_url: std::env::var("VIBESCAN_PUBLIC_RPC_URL").ok().filter(|s| !s.is_empty()),
            branch: std::env::var("VIBENET_BRANCH").unwrap_or_else(|_| "unknown".to_string()),
            commit: std::env::var("VIBENET_COMMIT").unwrap_or_else(|_| "unknown".to_string()),
        })
    }
}

fn require(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| eyre::eyre!("env var {name} is required"))
}

fn require_parsed<T>(name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = require(name)?;
    raw.parse::<T>().map_err(|err| eyre::eyre!("env var {name}={raw:?} is invalid: {err}"))
}

fn optional_parsed<T>(name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let res: Result<Option<T>> = match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => raw
            .parse::<T>()
            .map(Some)
            .map_err(|err| eyre::eyre!("env var {name}={raw:?} is invalid: {err}")),
        _ => Ok(None),
    };
    res.wrap_err_with(|| format!("reading env var {name}"))
}
