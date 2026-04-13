use std::collections::BTreeMap;

use aws_sdk_s3::Client as S3Client;
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};
use testcontainers_modules::minio::MinIO;

use base_snapshot::{
    Compressor, DatadirCatalog, ManifestBuilder, SnapshotStorage, UploadPlan,
};

const TEST_BUCKET: &str = "test-snapshots";

async fn setup_minio() -> (S3Client, testcontainers::ContainerAsync<MinIO>) {
    let container = MinIO::default()
        .start()
        .await
        .expect("failed to start MinIO container");

    let port = container.get_host_port_ipv4(9000).await.expect("failed to get MinIO port");
    let endpoint = format!("http://127.0.0.1:{port}");

    let config = aws_config::from_env()
        .endpoint_url(&endpoint)
        .region(aws_config::Region::new("us-east-1"))
        .credentials_provider(aws_credential_types::Credentials::new(
            "minioadmin",
            "minioadmin",
            None,
            None,
            "test",
        ))
        .load()
        .await;

    let s3_config = aws_sdk_s3::config::Builder::from(&config)
        .force_path_style(true)
        .build();

    let client = S3Client::from_conf(s3_config);

    client
        .create_bucket()
        .bucket(TEST_BUCKET)
        .send()
        .await
        .expect("failed to create test bucket");

    (client, container)
}

fn create_mock_datadir(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("db/mdbx.dat"), b"mock mdbx data for testing").unwrap();

    std::fs::create_dir_all(dir.join("rocksdb")).unwrap();
    std::fs::write(dir.join("rocksdb/000001.sst"), b"mock rocksdb sst").unwrap();

    std::fs::create_dir_all(dir.join("static_files")).unwrap();
    std::fs::write(
        dir.join("static_files/static_file_headers_0_499999_none_lz4"),
        b"headers chunk 0",
    )
    .unwrap();
    std::fs::write(
        dir.join("static_files/static_file_headers_500000_999999_none_lz4"),
        b"headers chunk 1",
    )
    .unwrap();
    std::fs::write(
        dir.join("static_files/static_file_transactions_0_499999_none_lz4"),
        b"transactions chunk 0",
    )
    .unwrap();
    std::fs::write(
        dir.join("static_files/static_file_receipts_0_499999_none_lz4"),
        b"receipts chunk 0",
    )
    .unwrap();
    std::fs::write(
        dir.join("static_files/static_file_account_changesets_0_499999_none_lz4"),
        b"account changesets chunk 0",
    )
    .unwrap();
}

fn create_mock_proofs_dir(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("mdbx.dat"), b"mock proofs mdbx data").unwrap();
}

#[tokio::test]
async fn full_snapshot_pipeline_with_minio() {
    let (client, _container) = setup_minio().await;
    let storage = SnapshotStorage::from_client(client, TEST_BUCKET.to_string());

    let tmp = tempfile::tempdir().unwrap();
    let datadir = tmp.path().join("datadir");
    let proofs_dir = tmp.path().join("proofs");
    create_mock_datadir(&datadir);
    create_mock_proofs_dir(&proofs_dir);

    // given: a catalog of the mock datadir with proofs
    let catalog = DatadirCatalog::from_paths(&datadir, Some(&proofs_dir)).unwrap();
    assert!(catalog.proofs_path.is_some());
    assert!(catalog.rocksdb_path.is_some());
    assert!(!catalog.static_chunks.is_empty());

    // given: no previous manifest exists
    let previous = storage.fetch_latest_manifest().await.unwrap();
    assert!(previous.is_none());

    // when: computing the upload plan against no previous manifest
    let plan = UploadPlan::from_diff(&catalog, None);
    assert!(plan.upload_state);
    assert!(plan.upload_rocksdb);
    assert!(plan.upload_proofs);
    let all_chunks = catalog.all_static_chunks();
    assert_eq!(plan.new_chunks.len(), all_chunks.len());

    // when: compressing and uploading all components
    let compressor = Compressor::new(1);
    let chunk_size = 5 * 1024 * 1024;

    let state_archive = compressor.compress_directory(&catalog.mdbx_path, "state.tar.zst").unwrap();
    storage
        .upload_archive("state.tar.zst", state_archive.data.clone(), chunk_size)
        .await
        .unwrap();

    let rocksdb_archive = compressor
        .compress_directory(catalog.rocksdb_path.as_ref().unwrap(), "rocksdb_indices.tar.zst")
        .unwrap();
    storage
        .upload_archive("rocksdb_indices.tar.zst", rocksdb_archive.data.clone(), chunk_size)
        .await
        .unwrap();

    let proofs_archive = compressor
        .compress_directory(catalog.proofs_path.as_ref().unwrap(), "proofs.tar.zst")
        .unwrap();
    storage
        .upload_archive("proofs.tar.zst", proofs_archive.data.clone(), chunk_size)
        .await
        .unwrap();

    for chunk in &plan.new_chunks {
        let files = catalog.source_files_for_chunk(chunk).unwrap();
        let archive_name = chunk.archive_name();
        let archive = compressor.compress_files(&files, &archive_name).unwrap();
        storage.upload_archive(&archive_name, archive.data, chunk_size).await.unwrap();
    }

    // when: building and publishing the manifest
    let mut builder =
        ManifestBuilder::new(999_999, 8453, "http://localhost".to_string(), 500_000);
    builder.add_single_component("state", "state.tar.zst", &state_archive, vec![]);
    builder.add_single_component(
        "rocksdb_indices",
        "rocksdb_indices.tar.zst",
        &rocksdb_archive,
        vec![],
    );
    builder.add_proofs_component("proofs.tar.zst", &proofs_archive, vec![]);

    let manifest = builder.build();
    storage.publish_manifest(&manifest).await.unwrap();

    // then: latest manifest is retrievable and correct
    let fetched = storage.fetch_latest_manifest().await.unwrap().unwrap();
    assert_eq!(fetched.block, 999_999);
    assert_eq!(fetched.chain_id, 8453);
    assert!(fetched.components.contains_key("state"));
    assert!(fetched.components.contains_key("rocksdb_indices"));
    assert!(fetched.components.contains_key("proofs"));

    // then: all archives exist in R2
    assert!(storage.object_exists("state.tar.zst").await.unwrap());
    assert!(storage.object_exists("rocksdb_indices.tar.zst").await.unwrap());
    assert!(storage.object_exists("proofs.tar.zst").await.unwrap());
    assert!(storage.object_exists("manifests/999999.json").await.unwrap());
    assert!(storage.object_exists("manifests/latest.json").await.unwrap());
}

#[tokio::test]
async fn incremental_snapshot_skips_existing_chunks() {
    let (client, _container) = setup_minio().await;
    let storage = SnapshotStorage::from_client(client, TEST_BUCKET.to_string());
    let compressor = Compressor::new(1);
    let chunk_size = 5 * 1024 * 1024;

    let tmp = tempfile::tempdir().unwrap();
    let datadir = tmp.path().join("datadir");
    create_mock_datadir(&datadir);

    // given: initial snapshot with 2 header chunks + 1 tx + 1 receipt + 1 changeset
    let catalog = DatadirCatalog::from_paths(&datadir, None).unwrap();
    let plan = UploadPlan::from_diff(&catalog, None);

    let mut builder =
        ManifestBuilder::new(999_999, 8453, "http://localhost".to_string(), 500_000);

    let state_archive = compressor.compress_directory(&catalog.mdbx_path, "state.tar.zst").unwrap();
    storage
        .upload_archive("state.tar.zst", state_archive.data.clone(), chunk_size)
        .await
        .unwrap();
    builder.add_single_component("state", "state.tar.zst", &state_archive, vec![]);

    for chunk in &plan.new_chunks {
        let files = catalog.source_files_for_chunk(chunk).unwrap();
        let archive_name = chunk.archive_name();
        let archive = compressor.compress_files(&files, &archive_name).unwrap();
        storage.upload_archive(&archive_name, archive.data, chunk_size).await.unwrap();
    }

    let manifest_v1 = builder.build();
    storage.publish_manifest(&manifest_v1).await.unwrap();

    // when: adding a new header chunk (simulating chain growth)
    std::fs::write(
        datadir.join("static_files/static_file_headers_1000000_1499999_none_lz4"),
        b"headers chunk 2",
    )
    .unwrap();

    // when: computing incremental diff
    let catalog_v2 = DatadirCatalog::from_paths(&datadir, None).unwrap();
    let plan_v2 = UploadPlan::from_diff(&catalog_v2, Some(&manifest_v1));

    // then: only the new chunk needs upload
    let new_header_chunks: Vec<_> = plan_v2
        .new_chunks
        .iter()
        .filter(|c| c.segment == base_snapshot::StaticSegment::Headers)
        .collect();
    assert_eq!(new_header_chunks.len(), 1);
    assert_eq!(new_header_chunks[0].range.start, 1_000_000);
    assert_eq!(new_header_chunks[0].range.end, 1_499_999);

    // then: state still needs re-upload (mutable)
    assert!(plan_v2.upload_state);
}

#[tokio::test]
async fn manifest_retention_keeps_last_n() {
    let (client, _container) = setup_minio().await;
    let storage = SnapshotStorage::from_client(client, TEST_BUCKET.to_string());

    // given: 5 versioned manifests
    for block in [100_000u64, 200_000, 300_000, 400_000, 500_000] {
        let manifest = base_snapshot::SnapshotManifest {
            block,
            chain_id: 8453,
            storage_version: 2,
            timestamp: 0,
            base_url: None,
            reth_version: None,
            components: BTreeMap::new(),
        };
        storage.publish_manifest(&manifest).await.unwrap();
    }

    // when: applying retention policy (keep last 2)
    storage.apply_retention(2).await.unwrap();

    // then: only the 2 most recent manifests remain (plus latest.json)
    let keys = storage.list_keys("manifests/").await.unwrap();
    let versioned: Vec<_> = keys.iter().filter(|k| *k != "manifests/latest.json").collect();
    assert_eq!(versioned.len(), 2);
    assert!(keys.contains(&"manifests/400000.json".to_string()));
    assert!(keys.contains(&"manifests/500000.json".to_string()));
    assert!(!keys.contains(&"manifests/100000.json".to_string()));
}

#[tokio::test]
async fn multipart_upload_large_data() {
    let (client, _container) = setup_minio().await;
    let storage = SnapshotStorage::from_client(client, TEST_BUCKET.to_string());

    // given: data larger than the chunk size
    let small_chunk_size = 5 * 1024 * 1024;
    let data = vec![42u8; small_chunk_size + 1024];

    // when: uploading
    storage
        .upload_archive("large-test.tar.zst", data.clone(), small_chunk_size)
        .await
        .unwrap();

    // then: the object exists
    assert!(storage.object_exists("large-test.tar.zst").await.unwrap());
}

#[tokio::test]
async fn object_exists_returns_false_for_missing() {
    let (client, _container) = setup_minio().await;
    let storage = SnapshotStorage::from_client(client, TEST_BUCKET.to_string());

    assert!(!storage.object_exists("does-not-exist.tar.zst").await.unwrap());
}
