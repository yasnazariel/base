#![doc = include_str!("../README.md")]

/// Stub Ethereum host bindings used by the imported ZK service.
pub mod host;
pub use host::{SingleChainOPSuccinctHost, StubHostArgs, StubWitnessGenerator};
