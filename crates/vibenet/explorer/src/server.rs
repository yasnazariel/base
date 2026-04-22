//! Axum HTTP server for vibescan. The handlers combine:
//!
//! - the local sqlite index (for listings + address activity), and
//! - on-demand reads from the upstream node (for full block/tx bodies,
//!   balances, code, etc.)
//!
//! No response is cached; SQLite is only used for the queries the node
//! cannot answer.

use crate::{
    config::ExplorerConfig,
    models::{
        ActivityItem, AddressDetail, BlockDetail, BlockListItem, PageCtx, StatsBlock, TxBlockMeta,
        TxDetail, TxListItem, format_eth,
    },
    rpc_proxy::RpcClient,
    storage::Storage,
    trace::TraceNode,
};
use alloy_primitives::{Address, B256};
use askama::Template;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use eyre::Result;
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;

/// Bundled CSS, baked in at build time so the container has nothing to mount.
const STYLE_CSS: &str = include_str!("../static/style.css");
/// Bundled favicon (Base blue square). Baked in so nothing has to be mounted
/// at runtime, same as the CSS.
const FAVICON_PNG: &[u8] = include_bytes!("../static/favicon.png");

#[derive(Clone)]
pub struct Explorer {
    state: Arc<AppState>,
}

pub struct AppState {
    storage: Storage,
    rpc: RpcClient,
    ctx: PageCtx,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("ctx", &self.ctx).finish_non_exhaustive()
    }
}

impl Explorer {
    pub fn new(config: &ExplorerConfig, storage: Storage, rpc: RpcClient) -> Self {
        let ctx = PageCtx {
            branch: config.branch.clone(),
            commit: config.commit.clone(),
            public_rpc_url: config.public_rpc_url.clone(),
        };
        Self { state: Arc::new(AppState { storage, rpc, ctx }) }
    }

    pub fn into_router(self) -> Router {
        Router::new()
            .route("/", get(home))
            .route("/block/{id}", get(block_detail))
            .route("/tx/{hash}", get(tx_detail))
            .route("/tx/{hash}/raw", get(tx_raw))
            .route("/tx/{hash}/receipt", get(tx_receipt_raw))
            .route("/tx/{hash}/trace", get(tx_trace))
            .route("/address/{addr}", get(address_detail))
            .route("/search", get(search))
            .route("/healthz", get(|| async { "ok" }))
            .route("/static/style.css", get(serve_css))
            .route("/static/favicon.png", get(serve_favicon))
            .route("/favicon.ico", get(serve_favicon))
            .route("/api/latest-blocks", get(api_latest_blocks))
            .route("/api/latest-txs", get(api_latest_txs))
            .route("/api/address/{addr}/activity", get(api_address_activity))
            .layer(TraceLayer::new_for_http())
            .with_state(self.state)
    }

    pub async fn serve(self, addr: SocketAddr, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "vibescan listening");
        let router = self.into_router();
        let mut shutdown = shutdown;
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown.changed().await;
            })
            .await?;
        Ok(())
    }
}

// --- templates -----------------------------------------------------------

#[derive(Template)]
#[template(path = "home.html")]
struct HomeTmpl {
    ctx: PageCtx,
    stats: StatsBlock,
    blocks: Vec<BlockListItem>,
    txs: Vec<TxListItem>,
}

#[derive(Template)]
#[template(path = "block.html")]
struct BlockTmpl {
    ctx: PageCtx,
    block: BlockDetail,
}

#[derive(Template)]
#[template(path = "tx.html")]
struct TxTmpl {
    ctx: PageCtx,
    tx: TxDetail,
}

#[derive(Template)]
#[template(path = "address.html")]
struct AddrTmpl {
    ctx: PageCtx,
    addr: AddressDetail,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTmpl {
    ctx: PageCtx,
    status: u16,
    message: String,
}

#[derive(Template)]
#[template(path = "raw.html")]
struct RawTmpl {
    ctx: PageCtx,
    title: String,
    subtitle: String,
    back_href: String,
    json: String,
}

#[derive(Template)]
#[template(path = "trace.html")]
struct TraceTmpl {
    ctx: PageCtx,
    tx_hash: String,
    back_href: String,
    /// Number of call frames (populated when [`Self::trace_html`] is set).
    total_calls: usize,
    /// Pre-rendered HTML for the call tree. Empty when [`Self::unavailable`]
    /// is set.
    trace_html: String,
    /// Friendly message shown instead of the tree when the upstream node
    /// couldn't (or wouldn't) produce a trace.
    unavailable: Option<String>,
}

// --- handlers ------------------------------------------------------------

async fn home(State(state): State<Arc<AppState>>) -> Response {
    let stats = match state.storage.stats().await {
        Ok(s) => s,
        Err(err) => return render_error(&state.ctx, 500, err.to_string()),
    };
    let head = state.rpc.block_number().await.unwrap_or(0);
    let blocks = state.storage.latest_blocks(15).await.unwrap_or_default();
    let txs = state.storage.latest_txs(15).await.unwrap_or_default();

    render_html(HomeTmpl {
        ctx: state.ctx.clone(),
        stats: StatsBlock::new(stats, head),
        blocks: blocks.into_iter().map(Into::into).collect(),
        txs: txs.into_iter().map(Into::into).collect(),
    })
}

async fn block_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let block = match parse_block_id(&id) {
        Ok(BlockLookup::Number(n)) => state.rpc.block_by_number(n).await,
        Ok(BlockLookup::Hash(h)) => state.rpc.block_by_hash(h).await,
        Err(err) => return render_error(&state.ctx, 400, err),
    };
    let block = match block {
        Ok(Some(b)) => b,
        Ok(None) => return render_error(&state.ctx, 404, format!("block {id} not found")),
        Err(err) => return render_error(&state.ctx, 502, err.to_string()),
    };

    let receipts = state
        .rpc
        .block_receipts(alloy_rpc_types_eth::BlockId::Hash(block.header.hash.into()))
        .await
        .ok()
        .flatten();

    render_html(BlockTmpl {
        ctx: state.ctx.clone(),
        block: BlockDetail::from_rpc(&block, receipts.as_deref()),
    })
}

async fn tx_detail(State(state): State<Arc<AppState>>, Path(hash): Path<String>) -> Response {
    let h = match parse_hash(&hash) {
        Ok(h) => h,
        Err(err) => return render_error(&state.ctx, 400, err),
    };
    let tx = match state.rpc.tx_by_hash(h).await {
        Ok(Some(t)) => t,
        Ok(None) => return render_error(&state.ctx, 404, format!("tx {hash} not found")),
        Err(err) => return render_error(&state.ctx, 502, err.to_string()),
    };
    // Receipt and block are both extra round-trips. Receipt carries fee /
    // gas / logs; the block carries timestamp + base fee. Failing either
    // just degrades the detail view — we still render a useful page with
    // whatever we have.
    let receipt = state.rpc.receipt(h).await.ok().flatten();
    let block_meta = match tx.inner.block_number {
        Some(n) => state.rpc.block_by_number(n).await.ok().flatten().map(|b| TxBlockMeta {
            timestamp: b.header.timestamp,
            base_fee_per_gas: b.header.base_fee_per_gas,
        }),
        None => None,
    };
    render_html(TxTmpl {
        ctx: state.ctx.clone(),
        tx: TxDetail::from_rpc(&tx, receipt.as_ref(), block_meta),
    })
}

async fn tx_raw(State(state): State<Arc<AppState>>, Path(hash): Path<String>) -> Response {
    let h = match parse_hash(&hash) {
        Ok(h) => h,
        Err(err) => return render_error(&state.ctx, 400, err),
    };
    match state.rpc.tx_by_hash(h).await {
        Ok(Some(tx)) => render_raw(
            &state.ctx,
            "Raw transaction".to_string(),
            hash.clone(),
            format!("/tx/{hash}"),
            serde_json::to_value(&tx).unwrap_or(serde_json::Value::Null),
        ),
        Ok(None) => render_error(&state.ctx, 404, format!("tx {hash} not found")),
        Err(err) => render_error(&state.ctx, 502, err.to_string()),
    }
}

async fn tx_receipt_raw(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> Response {
    let h = match parse_hash(&hash) {
        Ok(h) => h,
        Err(err) => return render_error(&state.ctx, 400, err),
    };
    match state.rpc.receipt(h).await {
        Ok(Some(r)) => render_raw(
            &state.ctx,
            "Raw receipt".to_string(),
            hash.clone(),
            format!("/tx/{hash}"),
            serde_json::to_value(&r).unwrap_or(serde_json::Value::Null),
        ),
        Ok(None) => render_error(&state.ctx, 404, format!("receipt for {hash} not found")),
        Err(err) => render_error(&state.ctx, 502, err.to_string()),
    }
}

async fn tx_trace(State(state): State<Arc<AppState>>, Path(hash): Path<String>) -> Response {
    let h = match parse_hash(&hash) {
        Ok(h) => h,
        Err(err) => return render_error(&state.ctx, 400, err),
    };
    // Upstream can fail for a bunch of legitimate reasons (debug namespace
    // disabled on the node, tx not yet mined, node still backfilling state)
    // plus a long tail of flakes. We don't want any of those to render a
    // 502 error page — they're expected and the user just wants to know the
    // trace isn't available right now. Fall back to a friendly notice and
    // keep the back-link so they can continue.
    match state.rpc.trace_transaction(h).await {
        Ok(v) => match TraceNode::from_json(&v) {
            Some(root) => {
                let total = root.total_calls();
                let html = root.render_html();
                render_html(TraceTmpl {
                    ctx: state.ctx.clone(),
                    tx_hash: hash.clone(),
                    back_href: format!("/tx/{hash}"),
                    total_calls: total,
                    trace_html: html,
                    unavailable: None,
                })
            }
            None => render_html(TraceTmpl {
                ctx: state.ctx.clone(),
                tx_hash: hash.clone(),
                back_href: format!("/tx/{hash}"),
                total_calls: 0,
                trace_html: String::new(),
                unavailable: Some(
                    "The node returned a trace in an unexpected format.".to_string(),
                ),
            }),
        },
        Err(err) => {
            tracing::warn!(error = %err, tx = %hash, "trace_transaction failed");
            render_html(TraceTmpl {
                ctx: state.ctx.clone(),
                tx_hash: hash.clone(),
                back_href: format!("/tx/{hash}"),
                total_calls: 0,
                trace_html: String::new(),
                unavailable: Some("Trace not available at this time.".to_string()),
            })
        }
    }
}

#[derive(Deserialize)]
struct AddrQuery {
    before: Option<String>,
}

async fn address_detail(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let address = match parse_address(&addr) {
        Ok(a) => a,
        Err(err) => return render_error(&state.ctx, 400, err),
    };

    let before = match q.before.as_deref().map(parse_cursor) {
        Some(Ok(cur)) => Some(cur),
        Some(Err(err)) => return render_error(&state.ctx, 400, err),
        None => None,
    };

    let (balance, nonce, code) = tokio::join!(
        state.rpc.balance(address),
        state.rpc.nonce(address),
        state.rpc.code(address),
    );

    let balance = balance.unwrap_or_default();
    let nonce = nonce.unwrap_or(0);
    let code = code.unwrap_or_default();

    let activity = state
        .storage
        .activity_for(address, before, 50)
        .await
        .unwrap_or_default();

    let next_cursor = activity.last().map(|a| format!("{}-{}-{}", a.block_num, a.tx_index, a.log_index));
    let activity_items: Vec<ActivityItem> = activity.into_iter().map(Into::into).collect();

    let addr_detail = AddressDetail {
        hex: format!("0x{}", hex::encode(address.as_slice())),
        balance_eth: format_eth(balance),
        nonce,
        is_contract: !code.is_empty(),
        code_size: code.len(),
        activity: activity_items,
        next_cursor,
    };

    render_html(AddrTmpl { ctx: state.ctx.clone(), addr: addr_detail })
}

#[derive(Deserialize)]
struct SearchQ {
    q: Option<String>,
}

async fn search(State(state): State<Arc<AppState>>, Query(q): Query<SearchQ>) -> Response {
    let term = q.q.unwrap_or_default();
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return Redirect::to("/").into_response();
    }
    // All-digits -> block number. 0x{40 hex} -> address. 0x{64 hex} -> hash.
    if trimmed.chars().all(|c: char| c.is_ascii_digit()) {
        return Redirect::to(&format!("/block/{trimmed}")).into_response();
    }
    let normalized = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    match normalized.len() {
        40 => Redirect::to(&format!("/address/0x{normalized}")).into_response(),
        64 => {
            // Could be a block hash or a tx hash; try tx first, fall through.
            if let Ok(Some(_)) =
                state.rpc.tx_by_hash(parse_hash(&format!("0x{normalized}")).unwrap_or_default()).await
            {
                return Redirect::to(&format!("/tx/0x{normalized}")).into_response();
            }
            Redirect::to(&format!("/block/0x{normalized}")).into_response()
        }
        _ => render_error(&state.ctx, 400, format!("unrecognized search term: {term}")),
    }
}

// --- JSON API ------------------------------------------------------------

async fn api_latest_blocks(State(state): State<Arc<AppState>>) -> Response {
    match state.storage.latest_blocks(25).await {
        Ok(rows) => {
            let items: Vec<BlockListItem> = rows.into_iter().map(Into::into).collect();
            Json(items.iter().map(block_to_json).collect::<Vec<_>>()).into_response()
        }
        Err(err) => error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn api_latest_txs(State(state): State<Arc<AppState>>) -> Response {
    match state.storage.latest_txs(25).await {
        Ok(rows) => {
            let items: Vec<TxListItem> = rows.into_iter().map(Into::into).collect();
            Json(items.iter().map(tx_to_json).collect::<Vec<_>>()).into_response()
        }
        Err(err) => error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn api_address_activity(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let address = match parse_address(&addr) {
        Ok(a) => a,
        Err(err) => return error_json(StatusCode::BAD_REQUEST, err),
    };
    let before = match q.before.as_deref().map(parse_cursor) {
        Some(Ok(c)) => Some(c),
        Some(Err(err)) => return error_json(StatusCode::BAD_REQUEST, err),
        None => None,
    };
    match state.storage.activity_for(address, before, 50).await {
        Ok(rows) => {
            let items: Vec<ActivityItem> = rows.into_iter().map(Into::into).collect();
            Json(items.iter().map(activity_to_json).collect::<Vec<_>>()).into_response()
        }
        Err(err) => error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

fn block_to_json(b: &BlockListItem) -> serde_json::Value {
    serde_json::json!({
        "number": b.number,
        "hash": b.hash.full,
        "timestamp": b.timestamp,
        "miner": b.miner.full,
        "tx_count": b.tx_count,
        "gas_used": b.gas_used,
        "gas_limit": b.gas_limit,
    })
}

fn tx_to_json(t: &TxListItem) -> serde_json::Value {
    serde_json::json!({
        "hash": t.hash.full,
        "block": t.block_num,
        "from": t.from.full,
        "to": t.to.as_ref().map(|a| a.full.clone()),
        "created": t.created.as_ref().map(|a| a.full.clone()),
        "value": t.value_eth,
        "status": t.status,
    })
}

fn activity_to_json(a: &ActivityItem) -> serde_json::Value {
    serde_json::json!({
        "block": a.block_num,
        "tx_hash": a.tx_hash_hex,
        "role": a.role,
        "detail": a.role_detail,
    })
}

// --- helpers -------------------------------------------------------------

enum BlockLookup {
    Number(u64),
    Hash(B256),
}

fn parse_block_id(id: &str) -> std::result::Result<BlockLookup, String> {
    let id = id.trim();
    if id.chars().all(|c: char| c.is_ascii_digit()) {
        return id.parse::<u64>().map(BlockLookup::Number).map_err(|e| e.to_string());
    }
    parse_hash(id).map(BlockLookup::Hash)
}

fn parse_hash(s: &str) -> std::result::Result<B256, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return Err(format!("expected 32-byte hash, got {} hex chars", s.len()));
    }
    let bytes = hex::decode(s).map_err(|e| format!("invalid hex: {e}"))?;
    Ok(B256::from_slice(&bytes))
}

fn parse_address(s: &str) -> std::result::Result<Address, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 40 {
        return Err(format!("expected 20-byte address, got {} hex chars", s.len()));
    }
    let bytes = hex::decode(s).map_err(|e| format!("invalid hex: {e}"))?;
    Ok(Address::from_slice(&bytes))
}

fn parse_cursor(s: &str) -> std::result::Result<(u64, u64, i64), String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err("cursor must be block-txIndex-logIndex".to_string());
    }
    let bn: u64 = parts[0].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let ti: u64 = parts[1].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let li: i64 = parts[2].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    Ok((bn, ti, li))
}

fn render_html<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(s) => Html(s).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {err}"))
            .into_response(),
    }
}

fn render_raw(
    ctx: &PageCtx,
    title: String,
    subtitle: String,
    back_href: String,
    value: serde_json::Value,
) -> Response {
    let json =
        serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("<serialize error: {e}>"));
    render_html(RawTmpl { ctx: ctx.clone(), title, subtitle, back_href, json })
}

fn render_error(ctx: &PageCtx, status: u16, message: String) -> Response {
    let body = ErrorTmpl { ctx: ctx.clone(), status, message: message.clone() };
    match body.render() {
        Ok(s) => (StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Html(s))
            .into_response(),
        Err(_) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            message,
        )
            .into_response(),
    }
}

fn error_json(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

async fn serve_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE_CSS).into_response()
}

/// Serve the bundled favicon. Wired under both `/static/favicon.png` (what
/// templates link to) and `/favicon.ico` (what browsers auto-request), both
/// returning the same PNG bytes.
async fn serve_favicon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        FAVICON_PNG,
    )
        .into_response()
}
