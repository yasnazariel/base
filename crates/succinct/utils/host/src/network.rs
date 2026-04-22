use anyhow::Result;
use sp1_sdk::network::{FulfillmentStrategy, signer::NetworkSigner};

/// Parses a stub fulfillment strategy from configuration.
pub fn parse_fulfillment_strategy(_: String) -> Result<FulfillmentStrategy> {
    todo!()
}

/// Builds a stub network signer.
pub async fn get_network_signer(_: bool) -> Result<NetworkSigner> {
    todo!()
}
