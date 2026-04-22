//! Lookup of contract addresses written by `vibenet-setup`.
//!
//! The setup container emits a flat JSON object at `/shared/contracts.json`
//! keyed by the contract names in `etc/vibenet/setup/contracts.yaml`
//! (e.g. `usdv`, `mockNft`), plus a few metadata fields. The faucet reads
//! this file on every USDV drip so it picks up new deploys without
//! restarting.

use std::fs;
use std::path::Path;

use alloy_primitives::Address;
use serde::Deserialize;

/// Read `path` and return the requested contract address, if present.
/// Returns `Ok(None)` when the file exists but the entry is missing (e.g.
/// setup is still running) so callers can distinguish that from a hard
/// filesystem error.
pub(crate) fn lookup(path: &Path, key: &str) -> eyre::Result<Option<Address>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(eyre::eyre!("reading {}: {e}", path.display())),
    };
    let raw: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&bytes)
        .map_err(|e| eyre::eyre!("parsing {}: {e}", path.display()))?;
    let Some(value) = raw.get(key) else { return Ok(None) };
    let s = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("{} in {} is not a string", key, path.display()))?;
    // Guard against the setup container writing metadata strings that
    // happen to share a key name (we use `_branch` / `_commit` prefixes,
    // but future additions shouldn't trip us up).
    let addr = s
        .parse::<Address>()
        .map_err(|e| eyre::eyre!("{} in {} is not an address: {e}", key, path.display()))?;
    Ok(Some(addr))
}

/// Typed view used in tests; kept here to document the on-disk shape.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ContractsFile {
    #[serde(rename = "_branch")]
    branch: Option<String>,
    #[serde(rename = "_commit")]
    commit: Option<String>,
    #[serde(rename = "faucetAddress")]
    faucet_address: Option<String>,
    usdv: Option<String>,
    nfv: Option<String>,
}
