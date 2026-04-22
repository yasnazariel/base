use alloy_primitives::B256;
use anyhow::Result;
use async_trait::async_trait;

use crate::witness_generation::WitnessGenerator;

/// Stub host interface for OP Succinct proof generation.
#[async_trait]
pub trait OPSuccinctHost: Send + Sync + 'static {
    /// Host argument type used by the stub implementation.
    type Args: Send + Sync + Clone + 'static;
    /// Witness generator type used by the stub implementation.
    type WitnessGenerator: WitnessGenerator + Send + Sync;

    /// Returns the witness generator instance.
    fn witness_generator(&self) -> &Self::WitnessGenerator;

    /// Fetches host arguments for the requested block range.
    async fn fetch(
        &self,
        l2_start: u64,
        l2_end: u64,
        l1_head: Option<B256>,
        safe_db_fallback: bool,
    ) -> Result<Self::Args>;

    /// Runs the stub host and returns stub witness data.
    async fn run(
        &self,
        args: &Self::Args,
    ) -> Result<<Self::WitnessGenerator as WitnessGenerator>::WitnessData>;
}
