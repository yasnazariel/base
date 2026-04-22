use alloy_primitives::{B256, Bytes};
use serde::{Deserialize, Serialize};

/// Stub boot info payload written into SP1 public values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BootInfoStruct {
    /// Stub L1 head hash.
    pub l1_head: B256,
    /// Stub pre-state output root.
    pub l2_pre_root: B256,
    /// Stub post-state output root.
    pub l2_post_root: B256,
    /// Stub starting L2 block number.
    pub l2_pre_block_number: u64,
    /// Stub ending L2 block number.
    pub l2_block_number: u64,
    /// Stub rollup config hash.
    pub rollup_config_hash: B256,
    /// Stub encoded intermediate roots.
    pub intermediate_roots: Bytes,
}
