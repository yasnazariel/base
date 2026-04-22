use alloy_consensus::Header;
use alloy_primitives::B256;
use anyhow::Result;
use base_succinct_client_utils::boot::BootInfoStruct;

/// Stub data fetcher satisfying the imported OP Succinct API.
#[derive(Debug)]
pub struct OPSuccinctDataFetcher {
    _private: (),
}

impl OPSuccinctDataFetcher {
    /// Creates a stub fetcher with placeholder rollup configuration.
    pub async fn new_with_rollup_config() -> Result<Self> {
        todo!()
    }

    /// Returns a stub latest L1 head for the provided boot infos.
    pub async fn get_latest_l1_head_in_batch(&self, _: &[BootInfoStruct]) -> Result<Header> {
        todo!()
    }

    /// Returns stub header preimages for the provided boot infos and head.
    pub async fn get_header_preimages(&self, _: &[BootInfoStruct], _: B256) -> Result<Vec<Header>> {
        todo!()
    }
}
