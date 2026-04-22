use std::sync::Arc;

use alloy_primitives::B256;
use anyhow::Result;
use async_trait::async_trait;
use base_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost, witness_generation::WitnessGenerator,
};
use sp1_sdk::SP1Stdin;

/// Stub witness generator for the Ethereum host adapter.
#[derive(Debug)]
pub struct StubWitnessGenerator;

impl WitnessGenerator for StubWitnessGenerator {
    type WitnessData = ();

    /// Converts stub witness data into SP1 stdin.
    fn get_sp1_stdin(&self, _: Self::WitnessData, _: u64) -> Result<SP1Stdin> {
        todo!()
    }
}

/// Stub host arguments for the Ethereum host adapter.
#[derive(Clone, Debug)]
pub struct StubHostArgs;

/// Stub OP Succinct host backed by a placeholder Ethereum DA fetcher.
#[derive(Clone, Debug)]
pub struct SingleChainOPSuccinctHost {
    /// Shared stub data fetcher.
    pub fetcher: Arc<OPSuccinctDataFetcher>,
    witness_generator: Arc<StubWitnessGenerator>,
}

#[async_trait]
impl OPSuccinctHost for SingleChainOPSuccinctHost {
    type Args = StubHostArgs;
    type WitnessGenerator = StubWitnessGenerator;

    fn witness_generator(&self) -> &Self::WitnessGenerator {
        &self.witness_generator
    }

    /// Fetches stub host arguments for the requested block range.
    async fn fetch(&self, _: u64, _: u64, _: Option<B256>, _: bool) -> Result<Self::Args> {
        todo!()
    }

    /// Runs the stub host and returns stub witness data.
    async fn run(
        &self,
        _: &Self::Args,
    ) -> Result<<Self::WitnessGenerator as WitnessGenerator>::WitnessData> {
        todo!()
    }
}

impl SingleChainOPSuccinctHost {
    /// Creates a new stub host from the provided fetcher.
    pub fn new(fetcher: Arc<OPSuccinctDataFetcher>) -> Self {
        Self { fetcher, witness_generator: Arc::new(StubWitnessGenerator) }
    }
}
