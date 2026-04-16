//! S3-backed storage for audit events using write-once semantics.
//!
//! Each audit event is stored as a separate S3 object keyed by
//! `bundles/{bundle_id}/{event_key}.json`. Duplicate writes are
//! rejected atomically by S3 via `If-None-Match: *` (HTTP 412),
//! eliminating the need for read-modify-write retry loops.

use std::{fmt, time::Instant};

use alloy_primitives::TxHash;
use anyhow::Result;
use async_trait::async_trait;
use aws_sdk_s3::{
    Client as S3Client, error::SdkError, operation::get_object::GetObjectError,
    primitives::ByteStream,
};
use base_bundles::AcceptedBundle;
use futures::future;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{
    metrics::Metrics,
    reader::Event,
    types::{BundleEvent, BundleId, DropReason, TransactionId},
};

/// Maximum number of retries for transient S3 errors (not conflicts).
const MAX_TRANSIENT_RETRIES: usize = 3;

/// Base delay in milliseconds between transient error retries.
const TRANSIENT_RETRY_BASE_DELAY_MS: u64 = 100;

/// S3 key types for storing different event types.
///
/// Each event is stored as its own object to avoid read-modify-write contention.
#[derive(Debug)]
pub enum S3Key<'a> {
    /// Key for a single bundle event: `bundles/{bundle_id}/{event_key}.json`
    BundleEvent {
        /// The bundle's unique identifier.
        bundle_id: BundleId,
        /// The event-specific key used for deduplication.
        event_key: &'a str,
    },
    /// Prefix for listing all events of a bundle: `bundles/{bundle_id}/`
    BundlePrefix(BundleId),
    /// Key for a transaction-to-bundle mapping: `transactions/by_hash/{tx_hash}/{bundle_id}.json`
    TransactionByHash {
        /// Transaction hash.
        tx_hash: TxHash,
        /// Bundle containing this transaction.
        bundle_id: BundleId,
    },
    /// Prefix for listing all bundles containing a transaction: `transactions/by_hash/{tx_hash}/`
    TransactionPrefix(TxHash),
}

impl fmt::Display for S3Key<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BundleEvent { bundle_id, event_key } => {
                write!(f, "bundles/{bundle_id}/{event_key}.json")
            }
            Self::BundlePrefix(bundle_id) => write!(f, "bundles/{bundle_id}/"),
            Self::TransactionByHash { tx_hash, bundle_id } => {
                write!(f, "transactions/by_hash/{tx_hash}/{bundle_id}.json")
            }
            Self::TransactionPrefix(tx_hash) => write!(f, "transactions/by_hash/{tx_hash}/"),
        }
    }
}

/// Metadata for a transaction, tracking which bundles it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransactionMetadata {
    /// Bundle IDs that contain this transaction.
    pub bundle_ids: Vec<BundleId>,
}

/// History event for a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum BundleHistoryEvent {
    /// Bundle was received.
    Received {
        /// Event key.
        key: String,
        /// Event timestamp.
        timestamp: i64,
        /// The accepted bundle.
        bundle: Box<AcceptedBundle>,
    },
    /// Bundle was cancelled.
    Cancelled {
        /// Event key.
        key: String,
        /// Event timestamp.
        timestamp: i64,
    },
    /// Bundle was included by a builder.
    BuilderIncluded {
        /// Event key.
        key: String,
        /// Event timestamp.
        timestamp: i64,
        /// Builder identifier.
        builder: String,
        /// Block number.
        block_number: u64,
        /// Flashblock index.
        flashblock_index: u64,
    },
    /// Bundle was included in a block.
    BlockIncluded {
        /// Event key.
        key: String,
        /// Event timestamp.
        timestamp: i64,
        /// Block number.
        block_number: u64,
        /// Block hash.
        block_hash: TxHash,
    },
    /// Bundle was dropped.
    Dropped {
        /// Event key.
        key: String,
        /// Event timestamp.
        timestamp: i64,
        /// Drop reason.
        reason: DropReason,
    },
}

impl BundleHistoryEvent {
    /// Returns the event key.
    pub fn key(&self) -> &str {
        match self {
            Self::Received { key, .. }
            | Self::Cancelled { key, .. }
            | Self::BuilderIncluded { key, .. }
            | Self::BlockIncluded { key, .. }
            | Self::Dropped { key, .. } => key,
        }
    }

    /// Returns the timestamp of the event.
    pub const fn timestamp(&self) -> i64 {
        match self {
            Self::Received { timestamp, .. }
            | Self::Cancelled { timestamp, .. }
            | Self::BuilderIncluded { timestamp, .. }
            | Self::BlockIncluded { timestamp, .. }
            | Self::Dropped { timestamp, .. } => *timestamp,
        }
    }
}

/// History of events for a bundle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleHistory {
    /// List of history events.
    pub history: Vec<BundleHistoryEvent>,
}

/// Converts a [`BundleEvent`] and its metadata into a [`BundleHistoryEvent`] for storage.
fn into_history_event(event: &Event) -> BundleHistoryEvent {
    match &event.event {
        BundleEvent::Received { bundle, .. } => BundleHistoryEvent::Received {
            key: event.key.clone(),
            timestamp: event.timestamp,
            bundle: bundle.clone(),
        },
        BundleEvent::Cancelled { .. } => {
            BundleHistoryEvent::Cancelled { key: event.key.clone(), timestamp: event.timestamp }
        }
        BundleEvent::BuilderIncluded { builder, block_number, flashblock_index, .. } => {
            BundleHistoryEvent::BuilderIncluded {
                key: event.key.clone(),
                timestamp: event.timestamp,
                builder: builder.clone(),
                block_number: *block_number,
                flashblock_index: *flashblock_index,
            }
        }
        BundleEvent::BlockIncluded { block_number, block_hash, .. } => {
            BundleHistoryEvent::BlockIncluded {
                key: event.key.clone(),
                timestamp: event.timestamp,
                block_number: *block_number,
                block_hash: *block_hash,
            }
        }
        BundleEvent::Dropped { reason, .. } => BundleHistoryEvent::Dropped {
            key: event.key.clone(),
            timestamp: event.timestamp,
            reason: reason.clone(),
        },
    }
}

/// Trait for writing bundle events to storage.
#[async_trait]
pub trait EventWriter {
    /// Archives a bundle event.
    async fn archive_event(&self, event: Event) -> Result<()>;
}

/// Trait for reading bundle events from S3.
#[async_trait]
pub trait BundleEventS3Reader {
    /// Gets the bundle history for a given bundle ID.
    async fn get_bundle_history(&self, bundle_id: BundleId) -> Result<Option<BundleHistory>>;
    /// Gets transaction metadata for a given transaction hash.
    async fn get_transaction_metadata(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionMetadata>>;
}

/// S3-backed event reader and writer using write-once semantics.
///
/// Each event is stored as a separate S3 object. Writes use `If-None-Match: *`
/// so that only the first writer succeeds; subsequent duplicate writes receive
/// a 412 Precondition Failed and are treated as successful no-ops.
#[derive(Clone, Debug)]
pub struct S3EventReaderWriter {
    s3_client: S3Client,
    bucket: String,
}

impl S3EventReaderWriter {
    /// Creates a new S3 event reader/writer.
    pub const fn new(s3_client: S3Client, bucket: String) -> Self {
        Self { s3_client, bucket }
    }

    /// Writes a single event object to S3 with write-once semantics.
    ///
    /// Uses `If-None-Match: *` so that only the first PUT for a given key
    /// succeeds. Returns `Ok(())` for both successful writes and duplicate
    /// 412 responses. Only retries on transient S3 errors (5xx, timeouts).
    async fn write_once(&self, key: &str, body: &[u8]) -> Result<()> {
        for attempt in 0..MAX_TRANSIENT_RETRIES {
            let put_start = Instant::now();
            let result = self
                .s3_client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .body(ByteStream::from(body.to_vec()))
                .if_none_match("*")
                .send()
                .await;

            Metrics::s3_put_duration().record(put_start.elapsed().as_secs_f64());

            match result {
                Ok(_) => {
                    debug!(s3_key = %key, "Wrote event object");
                    return Ok(());
                }
                Err(ref e) if is_conditional_check_failed(e) => {
                    Metrics::s3_write_conflicts().increment(1);
                    debug!(s3_key = %key, "Event already exists, skipping (412/409)");
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_TRANSIENT_RETRIES - 1 {
                        let delay = TRANSIENT_RETRY_BASE_DELAY_MS * 2_u64.pow(attempt as u32);
                        warn!(
                            s3_key = %key,
                            attempt = attempt + 1,
                            delay_ms = delay,
                            error = %e,
                            "Transient S3 error, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Failed to write {key} after {MAX_TRANSIENT_RETRIES} attempts: {e}"
                        ));
                    }
                }
            }
        }

        unreachable!("loop always returns")
    }

    /// Writes the bundle history event to S3.
    async fn write_bundle_event(&self, event: &Event) -> Result<()> {
        let bundle_id = event.event.bundle_id();
        let s3_key = S3Key::BundleEvent { bundle_id, event_key: &event.key }.to_string();
        let history_event = into_history_event(event);
        let body = serde_json::to_vec(&history_event)?;
        self.write_once(&s3_key, &body).await
    }

    /// Writes a transaction-to-bundle index entry to S3.
    async fn write_transaction_index(
        &self,
        tx_id: &TransactionId,
        bundle_id: BundleId,
    ) -> Result<()> {
        let s3_key = S3Key::TransactionByHash { tx_hash: tx_id.hash, bundle_id }.to_string();
        // The object content is minimal — just the bundle_id for discoverability.
        // The key itself encodes the relationship.
        let body = serde_json::to_vec(&bundle_id)?;
        self.write_once(&s3_key, &body).await
    }

    /// Lists all object keys under the given prefix.
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation_token = None;

        loop {
            let mut request = self.s3_client.list_objects_v2().bucket(&self.bucket).prefix(prefix);

            if let Some(token) = continuation_token.take() {
                request = request.continuation_token(token);
            }

            let response = request.send().await?;

            for obj in response.contents() {
                if let Some(key) = obj.key() {
                    keys.push(key.to_string());
                }
            }

            match response.next_continuation_token() {
                Some(token) => continuation_token = Some(token.to_string()),
                None => break,
            }
        }

        Ok(keys)
    }

    /// Gets a single object from S3 and deserializes it.
    async fn get_object<T>(&self, key: &str) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let get_start = Instant::now();
        let result = self.s3_client.get_object().bucket(&self.bucket).key(key).send().await;
        Metrics::s3_get_duration().record(get_start.elapsed().as_secs_f64());

        match result {
            Ok(response) => {
                let body = response.body.collect().await?;
                let value: T = serde_json::from_slice(&body.into_bytes())?;
                Ok(Some(value))
            }
            Err(e) => match &e {
                SdkError::ServiceError(service_err) => match service_err.err() {
                    GetObjectError::NoSuchKey(_) => Ok(None),
                    _ => Err(anyhow::anyhow!("Failed to get object {key}: {e}")),
                },
                _ => {
                    let error_string = e.to_string();
                    if error_string.contains("NoSuchKey")
                        || error_string.contains("NotFound")
                        || error_string.contains("404")
                    {
                        Ok(None)
                    } else {
                        Err(anyhow::anyhow!("Failed to get object {key}: {e}"))
                    }
                }
            },
        }
    }
}

#[async_trait]
impl EventWriter for S3EventReaderWriter {
    async fn archive_event(&self, event: Event) -> Result<()> {
        let bundle_id = event.event.bundle_id();
        let transaction_ids = event.event.transaction_ids();

        let bundle_start = Instant::now();
        let bundle_future = self.write_bundle_event(&event);

        let tx_futures: Vec<_> = transaction_ids
            .into_iter()
            .map(|tx_id| async move { self.write_transaction_index(&tx_id, bundle_id).await })
            .collect();

        tokio::try_join!(bundle_future, future::try_join_all(tx_futures))?;

        Metrics::update_bundle_history_duration().record(bundle_start.elapsed().as_secs_f64());

        Ok(())
    }
}

#[async_trait]
impl BundleEventS3Reader for S3EventReaderWriter {
    /// Reconstructs a bundle's full history by listing all event objects
    /// under `bundles/{bundle_id}/` and assembling them sorted by timestamp.
    async fn get_bundle_history(&self, bundle_id: BundleId) -> Result<Option<BundleHistory>> {
        let prefix = S3Key::BundlePrefix(bundle_id).to_string();
        let keys = self.list_keys(&prefix).await?;

        if keys.is_empty() {
            return Ok(None);
        }

        let get_futures: Vec<_> =
            keys.iter().map(|key| self.get_object::<BundleHistoryEvent>(key)).collect();

        let results = future::try_join_all(get_futures).await?;

        let mut history: Vec<BundleHistoryEvent> = results.into_iter().flatten().collect();
        history.sort_by_key(|e| e.timestamp());

        Ok(Some(BundleHistory { history }))
    }

    /// Reconstructs transaction metadata by listing all bundle mapping objects
    /// under `transactions/by_hash/{tx_hash}/` and extracting bundle IDs.
    async fn get_transaction_metadata(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionMetadata>> {
        let prefix = S3Key::TransactionPrefix(tx_hash).to_string();
        let keys = self.list_keys(&prefix).await?;

        if keys.is_empty() {
            return Ok(None);
        }

        // Extract bundle IDs from the S3 keys: transactions/by_hash/{tx_hash}/{bundle_id}.json
        let bundle_ids: Vec<BundleId> = keys
            .iter()
            .filter_map(|key| {
                let filename = key.rsplit('/').next()?;
                let uuid_str = filename.strip_suffix(".json")?;
                uuid_str.parse().ok()
            })
            .collect();

        if bundle_ids.is_empty() {
            return Ok(None);
        }

        Ok(Some(TransactionMetadata { bundle_ids }))
    }
}

/// Returns `true` if the S3 error indicates a conditional write conflict
/// (412 Precondition Failed or 409 `ConditionalRequestConflict`).
fn is_conditional_check_failed<E: std::fmt::Display>(err: &SdkError<E>) -> bool {
    match err {
        SdkError::ServiceError(service_err) => {
            let raw = service_err.raw();
            let status = raw.status().as_u16();
            status == 412 || status == 409
        }
        _ => {
            // Fallback: check error string for known conditional failure messages
            let error_string = err.to_string();
            error_string.contains("PreconditionFailed")
                || error_string.contains("ConditionalRequestConflict")
                || error_string.contains("412")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_key_bundle_event_format() {
        let bundle_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let key = S3Key::BundleEvent { bundle_id, event_key: "abc-123" }.to_string();
        assert_eq!(key, "bundles/550e8400-e29b-41d4-a716-446655440000/abc-123.json");
    }

    #[test]
    fn test_s3_key_bundle_prefix_format() {
        let bundle_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let key = S3Key::BundlePrefix(bundle_id).to_string();
        assert_eq!(key, "bundles/550e8400-e29b-41d4-a716-446655440000/");
    }

    #[test]
    fn test_s3_key_transaction_by_hash_format() {
        let bundle_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let tx_hash = TxHash::from([1u8; 32]);
        let key = S3Key::TransactionByHash { tx_hash, bundle_id }.to_string();
        assert_eq!(key, format!("transactions/by_hash/{tx_hash}/{bundle_id}.json"));
    }

    #[test]
    fn test_s3_key_transaction_prefix_format() {
        let tx_hash = TxHash::from([1u8; 32]);
        let key = S3Key::TransactionPrefix(tx_hash).to_string();
        assert_eq!(key, format!("transactions/by_hash/{tx_hash}/"));
    }

    #[test]
    fn test_into_history_event_received() {
        use base_bundles::{BundleExtensions, test_utils::create_bundle_from_txn_data};

        let bundle = create_bundle_from_txn_data();
        let bundle_id =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());
        let bundle_event = BundleEvent::Received { bundle_id, bundle: Box::new(bundle.clone()) };
        let event =
            Event { key: "test-key".to_string(), timestamp: 1234567890, event: bundle_event };

        let history_event = into_history_event(&event);
        match history_event {
            BundleHistoryEvent::Received { key, timestamp, bundle: b } => {
                assert_eq!(key, "test-key");
                assert_eq!(timestamp, 1234567890);
                assert_eq!(b.block_number, bundle.block_number);
            }
            _ => panic!("Expected Received event"),
        }
    }

    #[test]
    fn test_into_history_event_all_variants() {
        use base_bundles::{BundleExtensions, test_utils::create_bundle_from_txn_data};

        let bundle = create_bundle_from_txn_data();
        let bundle_id =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());

        // Received
        let event = Event {
            key: "k1".to_string(),
            timestamp: 100,
            event: BundleEvent::Received { bundle_id, bundle: Box::new(bundle) },
        };
        assert!(matches!(into_history_event(&event), BundleHistoryEvent::Received { .. }));

        // Cancelled
        let event = Event {
            key: "k2".to_string(),
            timestamp: 200,
            event: BundleEvent::Cancelled { bundle_id },
        };
        assert!(matches!(into_history_event(&event), BundleHistoryEvent::Cancelled { .. }));

        // BuilderIncluded
        let event = Event {
            key: "k3".to_string(),
            timestamp: 300,
            event: BundleEvent::BuilderIncluded {
                bundle_id,
                builder: "builder-1".to_string(),
                block_number: 12345,
                flashblock_index: 1,
            },
        };
        assert!(matches!(into_history_event(&event), BundleHistoryEvent::BuilderIncluded { .. }));

        // BlockIncluded
        let event = Event {
            key: "k4".to_string(),
            timestamp: 400,
            event: BundleEvent::BlockIncluded {
                bundle_id,
                block_number: 12345,
                block_hash: TxHash::from([1u8; 32]),
            },
        };
        assert!(matches!(into_history_event(&event), BundleHistoryEvent::BlockIncluded { .. }));

        // Dropped
        let event = Event {
            key: "k5".to_string(),
            timestamp: 500,
            event: BundleEvent::Dropped { bundle_id, reason: DropReason::TimedOut },
        };
        assert!(matches!(into_history_event(&event), BundleHistoryEvent::Dropped { .. }));
    }

    #[test]
    fn test_bundle_id_parsed_from_s3_key() {
        let bundle_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let key = format!("transactions/by_hash/0xabc/{bundle_id}.json");

        let filename = key.rsplit('/').next().unwrap();
        let uuid_str = filename.strip_suffix(".json").unwrap();
        let parsed: uuid::Uuid = uuid_str.parse().unwrap();
        assert_eq!(parsed, bundle_id);
    }
}
