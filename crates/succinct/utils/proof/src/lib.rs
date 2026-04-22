#![doc = include_str!("../README.md")]

use anyhow::Result;
use sp1_sdk::{SP1ProvingKey, SP1VerifyingKey};

/// Returns stub verifying keys for the imported ZK service.
pub async fn cluster_setup_vkeys() -> Result<(SP1VerifyingKey, SP1VerifyingKey)> {
    todo!()
}

/// Returns stub proving and verifying keys for the imported ZK service.
pub async fn cluster_setup_keys()
-> Result<(SP1ProvingKey, SP1VerifyingKey, SP1ProvingKey, SP1VerifyingKey)> {
    todo!()
}
