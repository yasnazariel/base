#![doc = include_str!("../README.md")]

/// Stub fetcher APIs used by the imported ZK service.
pub mod fetcher;
pub use fetcher::OPSuccinctDataFetcher;

/// Stub host traits used by the imported ZK service.
pub mod host;
pub use host::OPSuccinctHost;

/// Stub network helpers used by the imported ZK service.
pub mod network;
pub use network::{get_network_signer, parse_fulfillment_strategy};

mod proof;
pub use proof::get_agg_proof_stdin;

/// Stub witness generation traits used by the imported ZK service.
pub mod witness_generation;
pub use witness_generation::WitnessGenerator;
