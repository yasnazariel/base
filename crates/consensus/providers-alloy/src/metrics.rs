//! Metrics for the Alloy providers.

base_macros::define_metrics! {
    base_providers

    #[describe("Number of cache hits in chain provider")]
    #[label("cache", cache)]
    chain_cache_hits: gauge,

    #[describe("Number of cache misses in chain provider")]
    #[label("cache", cache)]
    chain_cache_misses: gauge,

    #[describe("Number of RPC calls made by chain provider")]
    #[label("method", method)]
    chain_rpc_calls: gauge,

    #[describe("Number of RPC errors in chain provider")]
    #[label("method", method)]
    chain_rpc_errors: gauge,

    #[describe("Number of requests made to beacon client")]
    #[label("method", method)]
    beacon_requests: gauge,

    #[describe("Number of errors in beacon client requests")]
    #[label("method", method)]
    beacon_errors: gauge,

    #[describe("Number of requests made to L2 chain provider")]
    #[label("method", method)]
    l2_chain_requests: gauge,

    #[describe("Number of errors in L2 chain provider requests")]
    #[label("method", method)]
    l2_chain_errors: gauge,

    #[describe("Number of blob sidecar fetches")]
    blob_fetches: gauge,

    #[describe("Number of blob sidecar fetch errors")]
    blob_fetch_errors: gauge,

    #[describe("Duration of provider requests in seconds")]
    #[label("method", method)]
    request_duration: histogram,

    #[describe("Number of active entries in provider caches")]
    #[label("cache", cache)]
    cache_entries: gauge,

    #[describe("Memory usage of provider caches in bytes")]
    #[label("cache", cache)]
    cache_memory_bytes: gauge,
}
