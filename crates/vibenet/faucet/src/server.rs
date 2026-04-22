//! HTTP server wiring for the vibenet faucet.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, Bytes, TxHash, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::TransactionRequest;
use alloy_sol_types::{SolCall, sol};
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::{
    contracts,
    state::{Asset, FaucetState},
};

// Minimal ABI for the USDV token. We only need `mint(address,uint256)` -
// the rest of the ERC-20 interface is exercised by users directly.
sol! {
    #[allow(missing_docs)]
    interface IUSDV {
        function mint(address to, uint256 amount) external;
    }
}

/// Header populated by Cloudflare that contains the true client IP.
const CF_CONNECTING_IP: &str = "cf-connecting-ip";

/// Top-level wrapper that owns the Tokio listener and the router.
#[derive(Debug)]
pub struct FaucetServer {
    listener: TcpListener,
    router: Router,
}

impl FaucetServer {
    /// Bind the TCP socket and construct the router.
    pub async fn bind(state: FaucetState) -> Result<Self> {
        let bind = state.config.bind;
        let listener = TcpListener::bind(bind).await?;
        info!(%bind, "faucet http server listening");
        let router = build_router(state);
        Ok(Self { listener, router })
    }

    /// Serve requests until the process is shut down.
    pub async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.router.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
        Ok(())
    }
}

fn build_router(state: FaucetState) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/drip", post(drip))
        .route("/drip-usdv", post(drip_usdv))
        .with_state(state)
}

#[derive(Serialize)]
struct StatusResponse {
    address: Address,
    chain_id: u64,
    drip_wei: U256,
    balance_wei: U256,
    ip_cooldown_secs: u64,
    addr_cooldown_secs: u64,
    /// USDV token address if vibenet-setup has deployed it and written
    /// `/shared/contracts.json`; `None` otherwise. Clients use this to
    /// decide whether to offer the USDV button.
    #[serde(skip_serializing_if = "Option::is_none")]
    usdv_address: Option<Address>,
    /// USDV minted per successful `/drip-usdv` call, in base units
    /// (6-decimal). Only meaningful when `usdv_address` is present.
    usdv_drip_units: U256,
}

async fn status(State(state): State<FaucetState>) -> Result<Json<StatusResponse>, ApiError> {
    let balance = state
        .provider
        .get_balance(state.config.address)
        .await
        .map_err(|e| ApiError::internal(format!("balance lookup failed: {e}")))?;

    // Contracts.json is produced by the one-shot setup container. If the
    // file is missing or the key is absent we silently omit the USDV row -
    // status is a polling endpoint and should never spam on that path.
    let usdv_address = contracts::lookup(&state.config.contracts_path, "usdv").ok().flatten();

    Ok(Json(StatusResponse {
        address: state.config.address,
        chain_id: state.config.chain_id,
        drip_wei: state.config.drip_wei,
        balance_wei: balance,
        ip_cooldown_secs: state.config.ip_cooldown.as_secs(),
        addr_cooldown_secs: state.config.addr_cooldown.as_secs(),
        usdv_address,
        usdv_drip_units: state.config.usdv_drip_units,
    }))
}

#[derive(Deserialize)]
struct DripRequest {
    address: String,
}

#[derive(Serialize)]
struct DripResponse {
    tx_hash: TxHash,
    amount_wei: U256,
    to: Address,
}

async fn drip(
    State(state): State<FaucetState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DripRequest>,
) -> Result<Json<DripResponse>, ApiError> {
    let to = Address::from_str(req.address.trim())
        .map_err(|_| ApiError::bad_request("invalid destination address"))?;

    let client_ip = client_ip(&headers, peer.ip());
    let ip_key = (Asset::Eth, client_ip);
    let addr_key = (Asset::Eth, to);

    if let Some(remaining) = state.ip_limiter.try_acquire(ip_key, state.config.ip_cooldown) {
        return Err(ApiError::rate_limited(format!(
            "ip cooldown active; retry in {}s",
            remaining.as_secs().max(1)
        )));
    }

    if let Some(remaining) = state.addr_limiter.try_acquire(addr_key, state.config.addr_cooldown) {
        state.ip_limiter.release(&ip_key);
        return Err(ApiError::rate_limited(format!(
            "address cooldown active; retry in {}s",
            remaining.as_secs().max(1)
        )));
    }

    let tx = TransactionRequest::default()
        .with_to(to)
        .with_value(state.config.drip_wei)
        .with_chain_id(state.config.chain_id);

    match state.provider.send_transaction(tx).await {
        Ok(pending) => {
            let tx_hash = *pending.tx_hash();
            info!(%to, %client_ip, %tx_hash, drip_wei = %state.config.drip_wei, "drip submitted");
            Ok(Json(DripResponse { tx_hash, amount_wei: state.config.drip_wei, to }))
        }
        Err(e) => {
            state.ip_limiter.release(&ip_key);
            state.addr_limiter.release(&addr_key);
            warn!(%to, %client_ip, error = %e, "drip submission failed");
            Err(ApiError::internal(format!("failed to submit drip: {e}")))
        }
    }
}

#[derive(Serialize)]
struct UsdvDripResponse {
    tx_hash: TxHash,
    token: Address,
    amount_units: U256,
    to: Address,
}

/// Mint `usdv_drip_units` of USDV to the requested address. USDV is a
/// public-mint ERC-20, so the faucet signer doesn't need to be the token
/// owner - we just go through the faucet for rate-limiting and to give
/// the UI a single button to press.
async fn drip_usdv(
    State(state): State<FaucetState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DripRequest>,
) -> Result<Json<UsdvDripResponse>, ApiError> {
    let to = Address::from_str(req.address.trim())
        .map_err(|_| ApiError::bad_request("invalid destination address"))?;

    // Resolve the token address on every call. The file is on a shared
    // volume and changes when vibenet-setup reruns (e.g. `just vibe` after
    // a branch switch); re-reading keeps the faucet in sync without a
    // restart. Reads are cheap (a few kB file) and only on the mint path.
    let token = contracts::lookup(&state.config.contracts_path, "usdv")
        .map_err(|e| ApiError::internal(format!("contracts lookup failed: {e}")))?
        .ok_or_else(|| {
            ApiError::service_unavailable(
                "USDV has not been deployed yet. Try again once vibenet-setup completes.",
            )
        })?;

    let client_ip = client_ip(&headers, peer.ip());
    let ip_key = (Asset::Usdv, client_ip);
    let addr_key = (Asset::Usdv, to);

    if let Some(remaining) = state.ip_limiter.try_acquire(ip_key, state.config.ip_cooldown) {
        return Err(ApiError::rate_limited(format!(
            "ip cooldown active; retry in {}s",
            remaining.as_secs().max(1)
        )));
    }

    if let Some(remaining) = state.addr_limiter.try_acquire(addr_key, state.config.addr_cooldown) {
        state.ip_limiter.release(&ip_key);
        return Err(ApiError::rate_limited(format!(
            "address cooldown active; retry in {}s",
            remaining.as_secs().max(1)
        )));
    }

    let amount = state.config.usdv_drip_units;
    let calldata = IUSDV::mintCall { to, amount }.abi_encode();
    let tx = TransactionRequest::default()
        .with_to(token)
        .with_input(Bytes::from(calldata))
        .with_chain_id(state.config.chain_id);

    match state.provider.send_transaction(tx).await {
        Ok(pending) => {
            let tx_hash = *pending.tx_hash();
            info!(%to, %client_ip, %token, %tx_hash, usdv_units = %amount, "usdv drip submitted");
            Ok(Json(UsdvDripResponse { tx_hash, token, amount_units: amount, to }))
        }
        Err(e) => {
            state.ip_limiter.release(&ip_key);
            state.addr_limiter.release(&addr_key);
            warn!(%to, %client_ip, %token, error = %e, "usdv drip submission failed");
            Err(ApiError::internal(format!("failed to submit usdv drip: {e}")))
        }
    }
}

/// Extract the real client IP. Prefers `CF-Connecting-IP` (set by the
/// Cloudflare edge and preserved through the nginx gateway) and falls back to
/// the direct TCP peer, which is the nginx container when we are deployed.
fn client_ip(headers: &HeaderMap, peer: IpAddr) -> IpAddr {
    if let Some(value) = headers.get(CF_CONNECTING_IP)
        && let Ok(s) = value.to_str()
        && let Ok(ip) = s.trim().parse::<IpAddr>()
    {
        return ip;
    }
    peer
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: msg.into() }
    }

    fn rate_limited(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::TOO_MANY_REQUESTS, message: msg.into() }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: msg.into() }
    }

    fn service_unavailable(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::SERVICE_UNAVAILABLE, message: msg.into() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}
