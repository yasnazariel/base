use anyhow::Result;
use sp1_sdk::SP1Stdin;

/// Stub witness generator interface used by the imported host bindings.
pub trait WitnessGenerator {
    /// Witness data type produced by the stub generator.
    type WitnessData;

    /// Converts stub witness data into SP1 stdin.
    fn get_sp1_stdin(&self, witness: Self::WitnessData, interval: u64) -> Result<SP1Stdin>;
}
