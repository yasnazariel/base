//! S3 event storage tests using write-once semantics.

use std::sync::Arc;

use alloy_primitives::TxHash;
use audit_archiver_lib::{
    BundleEvent, BundleEventS3Reader, Event, EventWriter, S3EventReaderWriter,
};
use tokio::task::JoinSet;
use uuid::Uuid;

mod common;
use base_bundles::{
    BundleExtensions,
    test_utils::{TXN_HASH, create_bundle_from_txn_data},
};
use common::TestHarness;

fn create_test_event(key: &str, timestamp: i64, bundle_event: BundleEvent) -> Event {
    Event { key: key.to_string(), timestamp, event: bundle_event }
}

#[tokio::test]
async fn system_test_event_write_and_read() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer = S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone());

    let bundle = create_bundle_from_txn_data();
    let bundle_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());
    let event = create_test_event(
        "test-key-1",
        1234567890,
        BundleEvent::Received { bundle_id, bundle: Box::new(bundle.clone()) },
    );

    writer.archive_event(event).await?;

    let bundle_history = writer.get_bundle_history(bundle_id).await?;
    assert!(bundle_history.is_some(), "bundle history should exist after write");

    let history = bundle_history.unwrap();
    assert_eq!(history.history.len(), 1, "exactly one event should be stored");
    assert_eq!(history.history[0].key(), "test-key-1");

    let metadata = writer.get_transaction_metadata(TXN_HASH).await?;
    assert!(metadata.is_some(), "transaction metadata should exist");

    let metadata = metadata.unwrap();
    assert!(metadata.bundle_ids.contains(&bundle_id), "bundle_id should be indexed");

    Ok(())
}

#[tokio::test]
async fn system_test_multiple_bundles_for_same_tx() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer = S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone());

    let bundle = create_bundle_from_txn_data();
    let bundle_id_one = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"bundle-one");
    let bundle_id_two = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"bundle-two");

    let event_one = create_test_event(
        "key-1",
        1234567890,
        BundleEvent::Received { bundle_id: bundle_id_one, bundle: Box::new(bundle.clone()) },
    );
    let event_two = create_test_event(
        "key-2",
        1234567891,
        BundleEvent::Received { bundle_id: bundle_id_two, bundle: Box::new(bundle.clone()) },
    );

    writer.archive_event(event_one).await?;
    writer.archive_event(event_two).await?;

    let metadata = writer.get_transaction_metadata(TXN_HASH).await?;
    assert!(metadata.is_some(), "transaction metadata should exist");

    let metadata = metadata.unwrap();
    assert_eq!(metadata.bundle_ids.len(), 2, "both bundle IDs should be indexed");
    assert!(metadata.bundle_ids.contains(&bundle_id_one));
    assert!(metadata.bundle_ids.contains(&bundle_id_two));

    Ok(())
}

#[tokio::test]
async fn system_test_events_appended() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer = S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone());

    let bundle = create_bundle_from_txn_data();
    let bundle_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());

    let events = [
        create_test_event(
            "test-key-1",
            1234567890,
            BundleEvent::Received { bundle_id, bundle: Box::new(bundle.clone()) },
        ),
        create_test_event("test-key-2", 1234567891, BundleEvent::Cancelled { bundle_id }),
    ];

    for (idx, event) in events.iter().enumerate() {
        writer.archive_event(event.clone()).await?;

        let bundle_history = writer.get_bundle_history(bundle_id).await?;
        assert!(bundle_history.is_some());

        let history = bundle_history.unwrap();
        assert_eq!(
            history.history.len(),
            idx + 1,
            "history should contain {0} events after writing {0}",
            idx + 1
        );
    }

    // Verify ordering is by timestamp
    let history = writer.get_bundle_history(bundle_id).await?.unwrap();
    assert_eq!(history.history[0].key(), "test-key-1");
    assert_eq!(history.history[1].key(), "test-key-2");

    Ok(())
}

#[tokio::test]
async fn system_test_event_deduplication() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer = S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone());

    let bundle = create_bundle_from_txn_data();
    let bundle_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());
    let event = create_test_event(
        "duplicate-key",
        1234567890,
        BundleEvent::Received { bundle_id, bundle: Box::new(bundle.clone()) },
    );

    // Write the same event twice — second should be a no-op via If-None-Match 412
    writer.archive_event(event.clone()).await?;
    writer.archive_event(event).await?;

    let bundle_history = writer.get_bundle_history(bundle_id).await?;
    assert!(bundle_history.is_some());

    let history = bundle_history.unwrap();
    assert_eq!(history.history.len(), 1, "duplicate event should not create second object");
    assert_eq!(history.history[0].key(), "duplicate-key");

    Ok(())
}

#[tokio::test]
async fn system_test_nonexistent_data() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer = S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone());

    let nonexistent_bundle_id = Uuid::parse_str("00000000-0000-0000-0000-000000000000").unwrap();
    let bundle_history = writer.get_bundle_history(nonexistent_bundle_id).await?;
    assert!(bundle_history.is_none(), "nonexistent bundle should return None");

    let nonexistent_tx_hash = TxHash::from([255u8; 32]);
    let metadata = writer.get_transaction_metadata(nonexistent_tx_hash).await?;
    assert!(metadata.is_none(), "nonexistent tx hash should return None");

    Ok(())
}

#[tokio::test]
#[ignore = "If-None-Match not supported by MinIO; test against real S3"]
async fn system_test_concurrent_writes_for_bundle() -> anyhow::Result<()> {
    let harness = TestHarness::new().await?;
    let writer =
        Arc::new(S3EventReaderWriter::new(harness.s3_client.clone(), harness.bucket_name.clone()));

    let bundle = create_bundle_from_txn_data();
    let bundle_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, bundle.bundle_hash().as_slice());

    // Write an initial event
    let event = create_test_event(
        "initial-key",
        1234567889i64,
        BundleEvent::Received { bundle_id, bundle: Box::new(bundle.clone()) },
    );
    writer.archive_event(event).await?;

    // Concurrently write 4 events: one shared key (dedup test) + 3 unique
    let mut join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();

    for i in 0..4 {
        let writer_clone = Arc::clone(&writer);
        let key = if i == 0 { "shared-key".to_string() } else { format!("unique-key-{i}") };

        let event =
            create_test_event(&key, 1234567890 + i as i64, BundleEvent::Cancelled { bundle_id });

        join_set.spawn(async move { writer_clone.archive_event(event).await });
    }

    // Also race a duplicate of "shared-key"
    let writer_clone = Arc::clone(&writer);
    let dup_event =
        create_test_event("shared-key", 1234567890, BundleEvent::Cancelled { bundle_id });
    join_set.spawn(async move { writer_clone.archive_event(dup_event).await });

    let results = join_set.join_all().await;
    assert_eq!(results.len(), 5, "all tasks should complete");
    for r in &results {
        assert!(r.is_ok(), "all writes should succeed (including 412 no-ops)");
    }

    let history = writer.get_bundle_history(bundle_id).await?.unwrap();

    // 1 initial + 1 shared + 3 unique = 5 events (duplicate shared-key is deduped)
    assert_eq!(history.history.len(), 5, "should have 5 unique events");

    let shared_count = history.history.iter().filter(|e| e.key() == "shared-key").count();
    assert_eq!(shared_count, 1, "duplicate shared-key should appear only once");

    Ok(())
}
