use std::{path::PathBuf, sync::Arc};

use alloy_genesis::ChainConfig;
use alloy_provider::RootProvider;
use base_common_network::Base;
use base_consensus_genesis::RollupConfig;
use base_consensus_providers::{OnlineBeaconClient, OnlineBlobProvider};
use base_proof_primitives::ProofRequest;
use serde::Serialize;

use crate::L1HeaderPrefetcher;

/// Default maximum concurrent L1 RPC requests across foreground and prefetch paths.
pub const DEFAULT_L1_CONCURRENCY: usize = 24;

/// The providers required for the host.
#[derive(Debug, Clone)]
pub struct HostProviders {
    /// The L1 EL provider.
    pub l1: RootProvider,
    /// The L1 beacon node provider.
    pub blobs: OnlineBlobProvider<OnlineBeaconClient>,
    /// The L2 EL provider.
    pub l2: RootProvider<Base>,
    /// KV-aware L1 header prefetcher.
    pub prefetcher: Arc<L1HeaderPrefetcher>,
}

/// Static infrastructure config — set once at startup, reused across proofs.
///
/// Constructed by the binary from CLI args or environment.
#[derive(Debug, Clone, Serialize)]
pub struct ProverConfig {
    /// L1 execution layer RPC URL.
    pub l1_eth_url: String,
    /// L2 execution layer RPC URL.
    pub l2_eth_url: String,
    /// L1 beacon API URL.
    pub l1_beacon_url: String,
    /// L2 chain ID.
    pub l2_chain_id: u64,
    /// Rollup configuration.
    pub rollup_config: RollupConfig,
    /// L1 chain configuration.
    pub l1_config: ChainConfig,
    /// Enables `debug_executePayload` for execution witness collection.
    pub enable_experimental_witness_endpoint: bool,
    /// Maximum concurrent L1 RPC requests for the proof host.
    pub l1_rpc_concurrency: usize,
    /// Number of parent L1 headers to speculatively prefetch when an
    /// `L1BlockHeader` hint is received.
    pub l1_prefetch_depth: usize,
}

/// Configuration for the proof host.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Per-proof parameters.
    pub request: ProofRequest,
    /// Static infrastructure config.
    pub prover: ProverConfig,
    /// Data directory for preimage data storage. When set, enables offline mode.
    pub data_dir: Option<PathBuf>,
}

impl HostConfig {
    /// Returns `true` if the host is running in offline mode.
    pub const fn is_offline(&self) -> bool {
        self.data_dir.is_some()
    }
}
