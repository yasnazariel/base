//! Snapshot manifest types and builder.
//!
//! Defines a manifest schema compatible with reth v2's `SnapshotManifest`. We define our own
//! types here because the pinned reth version (v1.11.3) predates the manifest-based download
//! system.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    catalog::{BlockRange, DatadirCatalog, StaticSegment},
    compress::CompressedArchive,
};

/// Top-level snapshot manifest describing all components at a given block height.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Block height this snapshot was taken at.
    pub block: u64,
    /// Chain ID (e.g. 8453 for Base mainnet).
    pub chain_id: u64,
    /// Storage format version (2 for reth storage v2).
    pub storage_version: u64,
    /// Unix timestamp when the snapshot was created.
    pub timestamp: u64,
    /// Base URL for downloading archives referenced in this manifest.
    pub base_url: Option<String>,
    /// Version of the tool that produced this manifest.
    pub reth_version: Option<String>,
    /// Components keyed by type (e.g. "state", "headers", "proofs").
    pub components: BTreeMap<String, ComponentManifest>,
}

/// Manifest entry for a single snapshot component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComponentManifest {
    /// A single archive file (state MDBX, `RocksDB`, proofs).
    Single(SingleArchive),
    /// A chunked component split across block-range archives (static files).
    Chunked(ChunkedArchive),
}

/// A single compressed archive file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleArchive {
    /// Filename of the archive.
    pub file: String,
    /// Compressed size in bytes.
    pub size: u64,
    /// Blake3 hash of the compressed archive.
    pub blake3: Option<String>,
    /// Checksums of individual files after extraction.
    pub output_files: Vec<OutputFileChecksum>,
}

/// A component split into block-range chunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkedArchive {
    /// Number of blocks per chunk file.
    pub blocks_per_file: u64,
    /// Total number of blocks covered.
    pub total_blocks: u64,
    /// Compressed sizes of each chunk, in order.
    pub chunk_sizes: Vec<u64>,
    /// Per-chunk output file checksums after extraction.
    pub chunk_output_files: Vec<Vec<OutputFileChecksum>>,
}

/// Blake3 checksum of a single file within an extracted archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputFileChecksum {
    /// Relative path within the extracted archive.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 hash of the file contents.
    pub blake3: String,
}

const PROOFS_COMPONENT_KEY: &str = "proofs";

/// Tracks compressed archives and assembles a reth-compatible `SnapshotManifest`.
#[derive(Debug)]
pub struct ManifestBuilder {
    block: u64,
    chain_id: u64,
    base_url: String,
    blocks_per_file: u64,
    components: BTreeMap<String, ComponentManifest>,
}

impl ManifestBuilder {
    /// Creates a new builder for a snapshot at the given block height.
    pub const fn new(block: u64, chain_id: u64, base_url: String, blocks_per_file: u64) -> Self {
        Self { block, chain_id, base_url, blocks_per_file, components: BTreeMap::new() }
    }

    /// Registers a compressed single-file component (state, rocksdb, proofs).
    pub fn add_single_component(
        &mut self,
        key: &str,
        filename: &str,
        archive: &CompressedArchive,
        output_files: Vec<OutputFileChecksum>,
    ) {
        let manifest = ComponentManifest::Single(SingleArchive {
            file: filename.to_string(),
            size: archive.data.len() as u64,
            blake3: Some(archive.blake3_hash.clone()),
            output_files,
        });
        self.components.insert(key.to_string(), manifest);
    }

    /// Registers the proofs extension MDBX as a component.
    pub fn add_proofs_component(
        &mut self,
        filename: &str,
        archive: &CompressedArchive,
        output_files: Vec<OutputFileChecksum>,
    ) {
        self.add_single_component(PROOFS_COMPONENT_KEY, filename, archive, output_files);
    }

    /// Registers a chunked static file component from individually compressed chunks.
    ///
    /// `chunks` must be sorted by block range.
    pub fn add_chunked_component(
        &mut self,
        segment: StaticSegment,
        chunks: &[(BlockRange, CompressedArchive, Vec<OutputFileChecksum>)],
    ) {
        if chunks.is_empty() {
            return;
        }

        let total_blocks = chunks.iter().map(|(r, _, _)| r.end - r.start + 1).sum();
        let chunk_sizes = chunks.iter().map(|(_, a, _)| a.data.len() as u64).collect();
        let chunk_output_files: Vec<Vec<OutputFileChecksum>> = chunks
            .iter()
            .map(|(_, _, files): &(_, _, Vec<OutputFileChecksum>)| files.clone())
            .collect();

        let manifest = ComponentManifest::Chunked(ChunkedArchive {
            blocks_per_file: self.blocks_per_file,
            total_blocks,
            chunk_sizes,
            chunk_output_files,
        });

        self.components.insert(segment.manifest_key().to_string(), manifest);
    }

    /// Carries over a component from a previous manifest (for unchanged static file segments).
    pub fn carry_over_component(&mut self, key: &str, manifest: ComponentManifest) {
        self.components.insert(key.to_string(), manifest);
    }

    /// Builds the final `SnapshotManifest`.
    pub fn build(self) -> SnapshotManifest {
        SnapshotManifest {
            block: self.block,
            chain_id: self.chain_id,
            storage_version: 2,
            timestamp: chrono::Utc::now().timestamp() as u64,
            base_url: Some(self.base_url),
            reth_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            components: self.components,
        }
    }

    /// Computes a `DatadirCatalog`'s static file component layout for chunked manifest entries.
    ///
    /// Returns a mapping of segment → sorted block ranges, matching what reth's manifest expects.
    pub fn segment_ranges_from_catalog(
        catalog: &DatadirCatalog,
    ) -> BTreeMap<StaticSegment, Vec<BlockRange>> {
        catalog.static_chunks.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_manifest_with_all_component_types() {
        let mut builder = ManifestBuilder::new(
            1_000_000,
            8453,
            "https://snapshots.base.org".to_string(),
            500_000,
        );

        let state_archive = CompressedArchive {
            data: vec![1; 100],
            blake3_hash: "abc123".to_string(),
            uncompressed_size: 200,
        };
        builder.add_single_component("state", "state.tar.zst", &state_archive, vec![]);

        let proofs_archive = CompressedArchive {
            data: vec![2; 50],
            blake3_hash: "def456".to_string(),
            uncompressed_size: 100,
        };
        builder.add_proofs_component("proofs.tar.zst", &proofs_archive, vec![]);

        let chunk_archive = CompressedArchive {
            data: vec![3; 80],
            blake3_hash: "ghi789".to_string(),
            uncompressed_size: 160,
        };
        builder.add_chunked_component(
            StaticSegment::Headers,
            &[(BlockRange { start: 0, end: 499_999 }, chunk_archive, vec![])],
        );

        let manifest = builder.build();

        assert_eq!(manifest.block, 1_000_000);
        assert_eq!(manifest.chain_id, 8453);
        assert_eq!(manifest.storage_version, 2);
        assert!(manifest.components.contains_key("state"));
        assert!(manifest.components.contains_key("proofs"));
        assert!(manifest.components.contains_key("headers"));
        assert_eq!(manifest.components.len(), 3);
    }

    #[test]
    fn carry_over_preserves_existing_component() {
        let mut builder = ManifestBuilder::new(
            2_000_000,
            8453,
            "https://snapshots.base.org".to_string(),
            500_000,
        );

        let existing = ComponentManifest::Chunked(ChunkedArchive {
            blocks_per_file: 500_000,
            total_blocks: 500_000,
            chunk_sizes: vec![1000],
            chunk_output_files: vec![vec![]],
        });

        builder.carry_over_component("headers", existing);

        let manifest = builder.build();
        assert!(manifest.components.contains_key("headers"));
    }
}
