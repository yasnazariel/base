//! Snapshot sidecar configuration.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the snapshot sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// How often to take snapshots, in hours.
    pub interval_hours: u64,
    /// Path to the reth datadir to snapshot.
    pub datadir: PathBuf,
    /// Path to the proofs extension MDBX directory.
    pub proofs_path: Option<PathBuf>,
    /// Local staging directory for copies before compression.
    pub staging_dir: PathBuf,
    /// Minimum block gap between snapshots.
    pub min_block_gap: u64,
    /// Docker container name to stop/start.
    pub container_name: String,
    /// Docker socket path.
    pub docker_socket: PathBuf,
    /// Graceful shutdown timeout in seconds.
    pub stop_timeout_secs: u64,
    /// S3-compatible endpoint URL.
    pub r2_endpoint: String,
    /// S3 bucket name.
    pub r2_bucket: String,
    /// S3 region (use "auto" for R2).
    pub r2_region: String,
    /// Concurrent upload streams.
    pub upload_concurrency: usize,
    /// Multipart upload chunk size in bytes.
    pub multipart_chunk_size: usize,
    /// Zstd compression level (1-22).
    pub compression_level: i32,
    /// Number of old snapshots to retain in R2.
    pub keep_last_n_snapshots: usize,
    /// JSON-RPC URL for pre-flight sync checks.
    pub rpc_url: String,
    /// Chain identifier (e.g. "base-mainnet").
    pub chain: String,
    /// Chain ID (e.g. 8453 for Base mainnet).
    pub chain_id: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            interval_hours: 72,
            datadir: PathBuf::from("/data/reth"),
            proofs_path: None,
            staging_dir: PathBuf::from("/snapshots/staging"),
            min_block_gap: 100_000,
            container_name: "base-reth".to_string(),
            docker_socket: PathBuf::from("/var/run/docker.sock"),
            stop_timeout_secs: 30,
            r2_endpoint: String::new(),
            r2_bucket: "base-snapshots".to_string(),
            r2_region: "auto".to_string(),
            upload_concurrency: 4,
            multipart_chunk_size: 128 * 1024 * 1024,
            compression_level: 3,
            keep_last_n_snapshots: 3,
            rpc_url: "http://localhost:8545".to_string(),
            chain: "base-mainnet".to_string(),
            chain_id: 8453,
        }
    }
}
