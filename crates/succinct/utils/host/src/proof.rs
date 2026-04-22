use alloy_consensus::Header;
use alloy_primitives::{Address, B256};
use anyhow::Result;
use base_succinct_client_utils::boot::BootInfoStruct;
use sp1_sdk::{SP1Proof, SP1Stdin, SP1VerifyingKey};

/// Builds stub aggregation proof stdin from placeholder proof inputs.
pub fn get_agg_proof_stdin(
    _: Vec<SP1Proof>,
    _: Vec<BootInfoStruct>,
    _: Vec<Header>,
    _: &SP1VerifyingKey,
    _: B256,
    _: Address,
) -> Result<SP1Stdin> {
    todo!()
}
