//! S3-compatible object storage client for snapshot upload/download to Cloudflare R2.

use aws_sdk_s3::{
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
    Client as S3Client,
};
use eyre::{bail, Result, WrapErr};
use tracing::{debug, info, warn};

use crate::{config::SnapshotConfig, manifest::SnapshotManifest};

const MANIFEST_PREFIX: &str = "manifests/";
const LATEST_MANIFEST_KEY: &str = "manifests/latest.json";

/// S3-compatible storage client for snapshot archives and manifests.
#[derive(Debug, Clone)]
pub struct SnapshotStorage {
    client: S3Client,
    bucket: String,
}

impl SnapshotStorage {
    /// Creates a new storage client from snapshot config.
    pub async fn from_config(config: &SnapshotConfig) -> Result<Self> {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .endpoint_url(&config.r2_endpoint)
            .region(aws_config::Region::new(config.r2_region.clone()))
            .load()
            .await;

        let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(true)
            .build();

        let client = S3Client::from_conf(s3_config);

        Ok(Self { client, bucket: config.r2_bucket.clone() })
    }

    /// Creates a storage client from an existing S3 client (useful for testing with `MinIO`).
    pub const fn from_client(client: S3Client, bucket: String) -> Self {
        Self { client, bucket }
    }

    /// Uploads a compressed archive to the given key.
    ///
    /// Uses multipart upload for data larger than `chunk_size`.
    pub async fn upload_archive(
        &self,
        key: &str,
        data: Vec<u8>,
        chunk_size: usize,
    ) -> Result<()> {
        let data_len = data.len();

        if data_len <= chunk_size {
            self.put_object(key, data).await?;
        } else {
            self.multipart_upload(key, data, chunk_size).await?;
        }

        info!(key, size = data_len, "uploaded archive");
        Ok(())
    }

    /// Checks whether an object exists at the given key.
    pub async fn object_exists(&self, key: &str) -> Result<bool> {
        match self.client.head_object().bucket(&self.bucket).key(key).send().await {
            Ok(_) => Ok(true),
            Err(err) => {
                let service_err = err.into_service_error();
                if service_err.is_not_found() {
                    Ok(false)
                } else {
                    Err(eyre::eyre!("head_object failed for {key}: {service_err}"))
                }
            }
        }
    }

    /// Lists all object keys under a given prefix.
    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation_token = None;

        loop {
            let mut req =
                self.client.list_objects_v2().bucket(&self.bucket).prefix(prefix);
            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.wrap_err("list_objects_v2 failed")?;

            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    keys.push(key.to_string());
                }
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(String::from);
            } else {
                break;
            }
        }

        Ok(keys)
    }

    /// Fetches the latest manifest from R2.
    pub async fn fetch_latest_manifest(&self) -> Result<Option<SnapshotManifest>> {
        match self.get_object(LATEST_MANIFEST_KEY).await {
            Ok(data) => {
                let manifest: SnapshotManifest =
                    serde_json::from_slice(&data).wrap_err("failed to parse manifest JSON")?;
                Ok(Some(manifest))
            }
            Err(_) => {
                debug!("no latest manifest found in R2");
                Ok(None)
            }
        }
    }

    /// Publishes a manifest to R2 with atomic ordering:
    /// 1. Upload versioned manifest (`manifests/{block}.json`)
    /// 2. Update `manifests/latest.json` to point to the new version
    pub async fn publish_manifest(&self, manifest: &SnapshotManifest) -> Result<()> {
        let json = serde_json::to_vec_pretty(manifest).wrap_err("failed to serialize manifest")?;

        let versioned_key = format!("{MANIFEST_PREFIX}{}.json", manifest.block);
        self.put_object(&versioned_key, json.clone()).await?;
        info!(key = %versioned_key, "uploaded versioned manifest");

        self.put_object(LATEST_MANIFEST_KEY, json).await?;
        info!("updated latest manifest pointer");

        Ok(())
    }

    /// Deletes objects under a versioned snapshot prefix (for retention cleanup).
    pub async fn delete_snapshot(&self, block: u64) -> Result<()> {
        let manifest_key = format!("{MANIFEST_PREFIX}{block}.json");
        self.delete_object(&manifest_key).await?;
        debug!(block, "deleted old snapshot manifest");
        Ok(())
    }

    /// Applies retention policy: keeps only the `keep_last_n` most recent manifests.
    pub async fn apply_retention(&self, keep_last_n: usize) -> Result<()> {
        let keys = self.list_keys(MANIFEST_PREFIX).await?;

        let mut versioned: Vec<u64> = keys
            .iter()
            .filter_map(|k| {
                k.strip_prefix(MANIFEST_PREFIX)?
                    .strip_suffix(".json")
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .collect();

        versioned.sort_unstable();

        if versioned.len() <= keep_last_n {
            return Ok(());
        }

        let to_delete = &versioned[..versioned.len() - keep_last_n];
        for &block in to_delete {
            if let Err(e) = self.delete_snapshot(block).await {
                warn!(block, error = %e, "failed to delete old snapshot");
            }
        }

        info!(
            deleted = to_delete.len(),
            retained = keep_last_n,
            "applied retention policy",
        );
        Ok(())
    }

    async fn put_object(&self, key: &str, data: Vec<u8>) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .send()
            .await
            .wrap_err_with(|| format!("put_object failed for {key}"))?;
        Ok(())
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .wrap_err_with(|| format!("get_object failed for {key}"))?;

        let data = resp
            .body
            .collect()
            .await
            .wrap_err("failed to read response body")?
            .into_bytes()
            .to_vec();
        Ok(data)
    }

    async fn delete_object(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .wrap_err_with(|| format!("delete_object failed for {key}"))?;
        Ok(())
    }

    async fn multipart_upload(
        &self,
        key: &str,
        data: Vec<u8>,
        chunk_size: usize,
    ) -> Result<()> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .wrap_err("create_multipart_upload failed")?;

        let upload_id = create
            .upload_id()
            .ok_or_else(|| eyre::eyre!("no upload_id returned"))?
            .to_string();

        let mut parts = Vec::new();
        let mut offset = 0usize;
        let mut part_number = 1i32;

        while offset < data.len() {
            let end = (offset + chunk_size).min(data.len());
            let chunk = data[offset..end].to_vec();

            let upload_part = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(ByteStream::from(chunk))
                .send()
                .await;

            match upload_part {
                Ok(resp) => {
                    let etag = resp.e_tag().unwrap_or_default().to_string();
                    parts.push(
                        CompletedPart::builder()
                            .part_number(part_number)
                            .e_tag(etag)
                            .build(),
                    );
                }
                Err(e) => {
                    let _ = self
                        .client
                        .abort_multipart_upload()
                        .bucket(&self.bucket)
                        .key(key)
                        .upload_id(&upload_id)
                        .send()
                        .await;
                    bail!("upload_part {part_number} failed for {key}: {e}");
                }
            }

            offset = end;
            part_number += 1;
        }

        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .wrap_err("complete_multipart_upload failed")?;

        Ok(())
    }
}
