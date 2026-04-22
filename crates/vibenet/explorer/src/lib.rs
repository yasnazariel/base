//! vibescan: a minimal block explorer for vibenet.
//!
//! The node already answers every read-by-hash/number query over JSON-RPC.
//! We only persist the one thing it cannot: the address -> activity index.
//! Everything else (block bodies, receipts, logs, balances, code, storage)
//! is fetched from the upstream RPC on demand, so the explorer stays thin
//! and easy to reset.

pub mod config;
pub mod indexer;
pub mod models;
pub mod rpc_proxy;
pub mod server;
pub mod storage;
pub mod trace;

pub use config::ExplorerConfig;
pub use server::Explorer;
