#![doc = include_str!("../README.md")]

mod error;
pub use error::{HostError, Result};

mod kv;
#[cfg(feature = "disk")]
pub use kv::DiskKeyValueStore;
pub use kv::{
    BootKeyValueStore, KeyValueStore, MemoryKeyValueStore, SharedKeyValueStore, SplitKeyValueStore,
    store_ordered_trie,
};

mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::ProofGuard;
pub use metrics::{Metrics, NoopProofGuard};

mod prefetch;
pub use prefetch::{DEFAULT_L1_CONCURRENCY, DEFAULT_PREFETCH_DEPTH, L1HeaderPrefetcher};

mod config;
pub use config::{HostConfig, HostProviders, ProverConfig};

mod host;
pub use host::Host;

mod server;
pub use server::PreimageServer;

mod handler;
pub use handler::{handle_hint, parse_blob_hint};

mod recording;
pub use recording::RecordingOracle;

mod backend;
pub use backend::{OfflineHostBackend, OnlineHostBackend};

mod service;
pub use service::{ProverError, ProverService};

#[cfg(feature = "precompiles")]
mod precompiles;
#[cfg(feature = "precompiles")]
pub use precompiles::execute;
