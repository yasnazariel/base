//! Metrics for the discovery service.

base_macros::define_metrics! {
    #[scope("base_node")]
    pub struct Metrics {
        #[describe("Events received by the discv5 service")]
        #[label("type", event_type)]
        discovery_events: gauge,

        #[describe("Requests made to find a node through the discv5 peer discovery service")]
        find_node_requests: gauge,

        #[describe("Observations of elapsed time to store ENRs in the on-disk bootstore")]
        enr_store_time: histogram,

        #[describe("Number of peers connected to the discv5 service")]
        discovery_peer_count: gauge,
    }
}
