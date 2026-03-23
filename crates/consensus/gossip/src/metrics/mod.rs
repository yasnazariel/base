//! Metrics for the Gossip stack.

base_metrics::define_metrics! {
    base_node

    #[describe("Events received by the gossip layer")]
    #[label("type", event_type)]
    gossip_events: gauge,

    #[describe("Connections made to the libp2p Swarm")]
    #[label("type", conn_type)]
    gossipsub_connection: gauge,

    #[describe("Number of OpNetworkPayloadEnvelope gossipped out through the libp2p Swarm")]
    unsafe_block_published: gauge,

    #[describe("Number of peers connected to the libp2p gossip Swarm")]
    swarm_peer_count: gauge,

    #[describe("Number of peers dialed by the libp2p Swarm")]
    dial_peer: gauge,

    #[describe("Number of errors when dialing peers")]
    #[label("reason", reason)]
    dial_peer_error: gauge,

    #[describe("Number of peers banned by the gossip stack")]
    banned_peers: gauge,

    #[describe("Calls made to the Gossip RPC module")]
    #[label("method", method)]
    rpc_calls: gauge,

    #[describe("Observations of peer scores in the gossipsub mesh")]
    peer_scores: histogram,

    #[describe("Total number of block validation attempts")]
    block_validation_total: counter,

    #[describe("Number of successful block validations")]
    block_validation_success: counter,

    #[describe("Number of failed block validations by reason")]
    #[label("reason", reason)]
    block_validation_failed: counter,

    #[describe("Duration of block validation in seconds")]
    block_validation_duration_seconds: histogram,

    #[describe("Distribution of block versions")]
    #[label("version", version)]
    block_version: counter,

    #[describe("Duration of peer connections in seconds")]
    gossip_peer_connection_duration_seconds: histogram,
}
