//! Metrics for the WebSocket publisher.

base_metrics::define_metrics_named! {
    PublishingMetrics, "base_builder",

    #[describe("Total messages sent to WebSocket subscribers")]
    ws_messages_sent_count: counter,

    #[describe("Number of active WebSocket connections")]
    ws_connections_active: gauge,

    #[describe("Total lagged WebSocket messages")]
    ws_lagged_count: counter,

    #[describe("WebSocket payload byte size")]
    ws_payload_byte_size: histogram,

    #[describe("Total WebSocket send errors")]
    ws_send_error_count: counter,

    #[describe("Total WebSocket handshake errors")]
    ws_handshake_error_count: counter,

    #[describe("WebSocket connection duration in seconds")]
    ws_connection_duration: histogram,
}
