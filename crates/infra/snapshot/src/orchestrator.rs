//! Snapshot orchestrator — coordinates the full snapshot lifecycle.

use std::path::Path;

use eyre::{Result, WrapErr};
use tracing::{error, info, warn};

use crate::{
    catalog::DatadirCatalog,
    compress::Compressor,
    config::SnapshotConfig,
    diff::UploadPlan,
    docker::DockerClient,
    manifest::{ComponentManifest, ManifestBuilder, OutputFileChecksum, SnapshotManifest},
    storage::SnapshotStorage,
};

/// Orchestrates the full snapshot pipeline: stop → copy → restart → compress → upload → publish.
#[derive(Debug)]
pub struct SnapshotOrchestrator {
    config: SnapshotConfig,
    storage: SnapshotStorage,
    docker: DockerClient,
    compressor: Compressor,
}

impl SnapshotOrchestrator {
    /// Creates a new orchestrator from config.
    pub async fn from_config(config: SnapshotConfig) -> Result<Self> {
        let storage = SnapshotStorage::from_config(&config).await?;
        let docker =
            DockerClient::new(config.container_name.clone(), config.stop_timeout_secs);
        let compressor = Compressor::new(config.compression_level);

        Ok(Self { config, storage, docker, compressor })
    }

    /// Creates an orchestrator with pre-built components (for testing).
    pub const fn new(
        config: SnapshotConfig,
        storage: SnapshotStorage,
        docker: DockerClient,
        compressor: Compressor,
    ) -> Self {
        Self { config, storage, docker, compressor }
    }

    /// Runs a single snapshot cycle.
    pub async fn run_snapshot(&self) -> Result<u64> {
        info!("starting snapshot cycle");

        let block = self.preflight_checks().await?;
        info!(block, "pre-flight passed");

        self.docker.stop().await?;

        let staging_dir = self.config.staging_dir.join(block.to_string());
        let copy_result = self.copy_datadir_to_staging(&staging_dir).await;

        self.docker.start().await?;

        copy_result?;
        info!(staging = %staging_dir.display(), "datadir copied to staging");

        let catalog = DatadirCatalog::from_paths(
            &staging_dir,
            self.config.proofs_path.as_deref().map(|_| staging_dir.join("proofs")).as_deref()
                .or(self.config.proofs_path.as_deref()),
        )?;

        let previous_manifest: Option<SnapshotManifest> =
            self.storage.fetch_latest_manifest().await?;
        let plan = UploadPlan::from_diff(&catalog, previous_manifest.as_ref());

        info!(
            new_chunks = plan.new_chunks.len(),
            carried = plan.carried_segments.len(),
            upload_state = plan.upload_state,
            upload_rocksdb = plan.upload_rocksdb,
            upload_proofs = plan.upload_proofs,
            total_uploads = plan.upload_count(),
            "computed upload plan",
        );

        let blocks_per_file = catalog
            .static_chunks
            .values()
            .flat_map(|ranges| ranges.iter().map(|r| r.end - r.start + 1))
            .next()
            .unwrap_or(500_000);

        let mut builder = ManifestBuilder::new(
            block,
            self.config.chain_id,
            self.config.r2_endpoint.clone(),
            blocks_per_file,
        );

        for (key, component) in &plan.carried_segments {
            builder.carry_over_component(key, ComponentManifest::clone(component));
        }

        if plan.upload_state {
            self.compress_and_upload_single(
                &catalog.mdbx_path,
                "state",
                "state.tar.zst",
                &mut builder,
            )
            .await?;
        }

        if plan.upload_rocksdb
            && let Some(rocksdb_path) = &catalog.rocksdb_path {
                self.compress_and_upload_single(
                    rocksdb_path,
                    "rocksdb_indices",
                    "rocksdb_indices.tar.zst",
                    &mut builder,
                )
                .await?;
            }

        if plan.upload_proofs
            && let Some(proofs_path) = &catalog.proofs_path {
                let archive = self.compressor.compress_directory(proofs_path, "proofs.tar.zst")?;
                let output_files = Self::collect_output_checksums(proofs_path)?;
                self.storage
                    .upload_archive("proofs.tar.zst", archive.data.clone(), self.config.multipart_chunk_size)
                    .await?;
                builder.add_proofs_component("proofs.tar.zst", &archive, output_files);
            }

        for chunk in &plan.new_chunks {
            let files = catalog.source_files_for_chunk(chunk)?;
            let archive_name = chunk.archive_name();
            let archive = self.compressor.compress_files(&files, &archive_name)?;

            self.storage
                .upload_archive(&archive_name, archive.data.clone(), self.config.multipart_chunk_size)
                .await?;
        }

        let manifest = builder.build();
        self.storage.publish_manifest(&manifest).await?;

        if self.config.keep_last_n_snapshots > 0
            && let Err(e) = self.storage.apply_retention(self.config.keep_last_n_snapshots).await {
                warn!(error = %e, "retention cleanup failed");
            }

        self.cleanup_staging(&staging_dir)?;

        info!(block, "snapshot cycle complete");
        Ok(block)
    }

    /// Runs the sidecar loop, taking snapshots at the configured interval.
    pub async fn run_loop(&self) -> Result<()> {
        let interval = std::time::Duration::from_secs(self.config.interval_hours * 3600);

        loop {
            match self.run_snapshot().await {
                Ok(block) => info!(block, "snapshot succeeded"),
                Err(e) => error!(error = %e, "snapshot failed"),
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn preflight_checks(&self) -> Result<u64> {
        let client = reqwest::Client::new();
        let resp: reqwest::Response = client
            .post(&self.config.rpc_url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1
            }))
            .send()
            .await
            .wrap_err("pre-flight RPC failed")?;

        let body: serde_json::Value = resp.json().await?;
        let hex = body["result"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("unexpected RPC response: {body}"))?;

        let block = u64::from_str_radix(hex.trim_start_matches("0x"), 16)?;
        Ok(block)
    }

    async fn copy_datadir_to_staging(&self, staging_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(staging_dir)
            .wrap_err_with(|| format!("failed to create staging dir {}", staging_dir.display()))?;

        copy_dir_recursive(&self.config.datadir, staging_dir)?;

        if let Some(proofs_path) = &self.config.proofs_path
            && proofs_path.exists() {
                let proofs_staging = staging_dir.join("proofs");
                copy_dir_recursive(proofs_path, &proofs_staging)?;
            }

        Ok(())
    }

    async fn compress_and_upload_single(
        &self,
        dir: &Path,
        component_key: &str,
        archive_name: &str,
        builder: &mut ManifestBuilder,
    ) -> Result<()> {
        let archive = self.compressor.compress_directory(dir, archive_name)?;
        let output_files = Self::collect_output_checksums(dir)?;

        self.storage
            .upload_archive(archive_name, archive.data.clone(), self.config.multipart_chunk_size)
            .await?;

        builder.add_single_component(component_key, archive_name, &archive, output_files);
        Ok(())
    }

    fn collect_output_checksums(dir: &Path) -> Result<Vec<OutputFileChecksum>> {
        let mut checksums = Vec::new();

        for entry in walkdir(dir)? {
            let path = entry;
            if path.is_file() {
                let data = std::fs::read(&path)?;
                let hash = blake3::hash(&data).to_hex().to_string();
                let relative = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                checksums.push(OutputFileChecksum {
                    path: relative,
                    size: data.len() as u64,
                    blake3: hash,
                });
            }
        }

        Ok(checksums)
    }

    fn cleanup_staging(&self, staging_dir: &Path) -> Result<()> {
        if staging_dir.exists() {
            std::fs::remove_dir_all(staging_dir)
                .wrap_err_with(|| format!("failed to clean up {}", staging_dir.display()))?;
        }
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn walkdir(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path)?);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}
