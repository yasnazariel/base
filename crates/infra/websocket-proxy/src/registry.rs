use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use axum::extract::ws::Message;
use futures::{SinkExt, stream::StreamExt};
use serde_json::Value;
use tokio::{
    sync::broadcast::{Sender, error::RecvError},
    time::{Duration, interval, timeout},
};
use tracing::{debug, info, trace, warn};

use crate::{client::ClientConnection, metrics::Metrics};

/// A broadcast message that lazily caches the decompressed, parsed JSON representation.
///
/// The `OnceLock` ensures that decompression and JSON parsing happen at most once per
/// broadcast message, regardless of how many subscriber tasks race to filter it. All
/// subscribers that need the parsed form share the same `Arc<Value>` via the lock.
#[derive(Debug)]
pub struct BroadcastMessage {
    /// The raw WebSocket message as received from the upstream source.
    pub message: Message,
    /// Lazily initialised parsed JSON. `None` inside the `OnceLock` means the payload
    /// could not be decompressed or parsed. Uninitialised means no subscriber has needed
    /// it yet (or the subscriber has a [`FilterType::None`] filter).
    cached_json: OnceLock<Option<Arc<Value>>>,
}

impl BroadcastMessage {
    /// Wraps a raw [`Message`] in a broadcast envelope with an empty JSON cache.
    pub fn new(message: Message) -> Arc<Self> {
        Arc::new(Self { message, cached_json: OnceLock::new() })
    }

    /// Returns a reference to the shared JSON cache used by [`FilterType::matches_with_cache`].
    pub fn cached_json(&self) -> &OnceLock<Option<Arc<Value>>> {
        &self.cached_json
    }
}

fn get_message_size(msg: &Message) -> u64 {
    match msg {
        Message::Text(text) => text.len() as u64,
        Message::Binary(data) | Message::Ping(data) | Message::Pong(data) => data.len() as u64,
        Message::Close(_) => 0,
    }
}

/// Manages broadcast subscriptions for connected WebSocket clients.
#[derive(Clone, Debug)]
pub struct Registry {
    sender: Sender<Arc<BroadcastMessage>>,
    metrics: Arc<Metrics>,
    compressed: bool,
    ping_enabled: bool,
    pong_timeout_ms: u64,
    send_timeout_ms: Duration,
}

impl Registry {
    /// Creates a new registry with the given broadcast sender and configuration.
    pub const fn new(
        sender: Sender<Arc<BroadcastMessage>>,
        metrics: Arc<Metrics>,
        compressed: bool,
        ping_enabled: bool,
        pong_timeout_ms: u64,
        send_timeout_ms: Duration,
    ) -> Self {
        Self { sender, metrics, compressed, ping_enabled, pong_timeout_ms, send_timeout_ms }
    }

    /// Subscribes a client to the broadcast channel and forwards matching messages.
    pub async fn subscribe(&self, client: ClientConnection) {
        info!(message = "subscribing client", client = client.id());

        let mut receiver = self.sender.subscribe();
        let metrics = Arc::clone(&self.metrics);
        metrics.new_connections.increment(1);

        let filter = client.filter.clone();
        let compressed = self.compressed;
        let client_id = client.id();
        let (mut ws_sender, ws_receiver) = client.websocket.split();

        let (pong_error_tx, mut pong_error_rx) = tokio::sync::oneshot::channel();
        let client_reader = self.start_reader(ws_receiver, client_id.clone(), pong_error_tx);

        loop {
            tokio::select! {
                broadcast_result = receiver.recv() => {
                    match broadcast_result {
                        Ok(broadcast_msg) => {
                            let msg_bytes = match &broadcast_msg.message {
                                Message::Binary(data) => data.as_ref(),
                                _ => &[],
                            };

                            // matches_with_cache decompresses and parses the JSON at most once
                            // across all concurrent subscribers for this message. FilterType::None
                            // short-circuits without touching the cache.
                            if filter.matches_with_cache(
                                msg_bytes,
                                compressed,
                                broadcast_msg.cached_json(),
                            ) {
                                trace!(
                                    message = "filter matched for client",
                                    client = client_id,
                                    filter = ?filter
                                );

                                let send_start = Instant::now();
                                let msg_size = get_message_size(&broadcast_msg.message);
                                let msg_clone = broadcast_msg.message.clone();
                                let send_result =
                                    timeout(self.send_timeout_ms, ws_sender.send(msg_clone)).await;
                                let send_duration = send_start.elapsed();

                                metrics.message_send_duration.record(send_duration);

                                match send_result {
                                    Ok(Ok(())) => {
                                        // Success - message sent
                                        trace!(
                                            message = "message sent to client",
                                            client = client_id
                                        );
                                        metrics.sent_messages.increment(1);
                                        metrics.bytes_broadcasted.increment(msg_size);
                                    }
                                    Ok(Err(e)) => {
                                        // Send failed (connection error)
                                        warn!(
                                            message = "failed to send data to client",
                                            client = client_id,
                                            error = e.to_string()
                                        );
                                        metrics.failed_messages.increment(1);
                                        break;
                                    }
                                    Err(_) => {
                                        // Timeout - client too slow
                                        warn!(
                                            message = "send timeout - disconnecting slow client",
                                            client = client_id,
                                            timeout_ms = self.send_timeout_ms.as_millis()
                                        );
                                        metrics.failed_messages.increment(1);
                                        break;
                                    }
                                }
                            } else {
                                trace!(client_id = %client_id, "Filter did not match");
                            }
                        }
                        Err(RecvError::Closed) => {
                            info!(message = "upstream connection closed", client = client_id);
                            break;
                        }
                        Err(RecvError::Lagged(_)) => {
                            info!(message = "client is lagging", client = client_id);
                            metrics.lagged_connections.increment(1);
                            break;
                        }
                    }
                }

                _ = &mut pong_error_rx => {
                    debug!(message = "client reader signaled disconnect", client = client_id);
                    break;
                }
            }
        }

        client_reader.abort();
        metrics.closed_connections.increment(1);

        info!(message = "client disconnected", client = client_id);
    }

    fn start_reader(
        &self,
        ws_receiver: futures::stream::SplitStream<axum::extract::ws::WebSocket>,
        client_id: String,
        pong_error_tx: tokio::sync::oneshot::Sender<()>,
    ) -> tokio::task::JoinHandle<()> {
        let ping_enabled = self.ping_enabled;
        let pong_timeout_ms = self.pong_timeout_ms;
        let metrics = Arc::clone(&self.metrics);

        tokio::spawn(async move {
            let mut ws_receiver = ws_receiver;
            let mut last_pong = Instant::now();
            let mut timeout_checker = interval(Duration::from_millis(pong_timeout_ms / 4));
            let pong_timeout = Duration::from_millis(pong_timeout_ms);

            loop {
                tokio::select! {
                    msg = ws_receiver.next() => {
                        match msg {
                            Some(Ok(Message::Pong(_))) => {
                                if ping_enabled {
                                    trace!(
                                        message = "received pong from client",
                                        client = client_id
                                    );
                                    last_pong = Instant::now();
                                }
                            }
                            Some(Ok(Message::Close(_))) => {
                                trace!(
                                    message = "received close from client",
                                    client = client_id
                                );
                                let _ = pong_error_tx.send(());
                                return;
                            }
                            Some(Err(e)) => {
                                trace!(
                                    message = "error receiving from client",
                                    client = client_id,
                                    error = e.to_string()
                                );
                                let _ = pong_error_tx.send(());
                                return;
                            }
                            None => {
                                trace!(message = "client connection closed", client = client_id);
                                let _ = pong_error_tx.send(());
                                return;
                            }
                            _ => {}
                        }
                    }

                    _ = timeout_checker.tick() => {
                        if ping_enabled && last_pong.elapsed() > pong_timeout  {
                            debug!(
                                message = "client pong timeout, disconnecting",
                                client = client_id,
                                elapsed_ms = last_pong.elapsed().as_millis()
                            );
                            metrics.client_pong_disconnects.increment(1);
                            let _ = pong_error_tx.send(());
                            return;
                        }
                    }
                }
            }
        })
    }
}
