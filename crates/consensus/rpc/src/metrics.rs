//! Metrics for the consensus RPC module.

base_macros::define_metrics! {
    rollup

    #[describe("Calls made to the Rollup RPC module")]
    #[label("method", method)]
    rpc: gauge,
}
