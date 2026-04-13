//! Diff engine — compares a datadir catalog against the previous manifest to produce an upload plan.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    catalog::{DatadirCatalog, StaticFileChunk, StaticSegment},
    manifest::{ComponentManifest, SnapshotManifest},
};

/// Describes what needs to be uploaded for a new snapshot.
#[derive(Debug)]
pub struct UploadPlan {
    /// Static file chunks that are new and need compression + upload.
    pub new_chunks: Vec<StaticFileChunk>,
    /// Static file segments carried over from the previous manifest (already in R2).
    pub carried_segments: BTreeMap<String, ComponentManifest>,
    /// Whether `state.tar.zst` needs upload (always true).
    pub upload_state: bool,
    /// Whether `rocksdb_indices.tar.zst` needs upload.
    pub upload_rocksdb: bool,
    /// Whether `proofs.tar.zst` needs upload.
    pub upload_proofs: bool,
}

impl UploadPlan {
    /// Diff a catalog against a previous manifest to determine what's new.
    ///
    /// Static file chunks that already exist in the previous manifest (same segment + range)
    /// are carried over. Everything else needs fresh compression and upload.
    pub fn from_diff(catalog: &DatadirCatalog, previous: Option<&SnapshotManifest>) -> Self {
        let current_chunks: BTreeSet<_> = catalog.all_static_chunks().into_iter().collect();

        let (new_chunks, carried_segments) = match previous {
            Some(manifest) => Self::diff_against_manifest(&current_chunks, catalog, manifest),
            None => (current_chunks.into_iter().collect(), BTreeMap::new()),
        };

        Self {
            new_chunks,
            carried_segments,
            upload_state: true,
            upload_rocksdb: catalog.rocksdb_path.is_some(),
            upload_proofs: catalog.proofs_path.is_some(),
        }
    }

    /// Returns the total number of archives that need uploading (new chunks + mutable components).
    pub const fn upload_count(&self) -> usize {
        let mut count = self.new_chunks.len();
        if self.upload_state {
            count += 1;
        }
        if self.upload_rocksdb {
            count += 1;
        }
        if self.upload_proofs {
            count += 1;
        }
        count
    }

    fn diff_against_manifest(
        current_chunks: &BTreeSet<StaticFileChunk>,
        catalog: &DatadirCatalog,
        manifest: &SnapshotManifest,
    ) -> (Vec<StaticFileChunk>, BTreeMap<String, ComponentManifest>) {
        let mut existing_chunks: BTreeSet<StaticFileChunk> = BTreeSet::new();
        let mut carried = BTreeMap::new();

        for (key, component) in &manifest.components {
            let Some(segment) = StaticSegment::from_manifest_key(key) else {
                continue;
            };

            if let ComponentManifest::Chunked(chunked) = component {
                let mut block = 0u64;
                for _ in 0..chunked.chunk_sizes.len() {
                    let range = crate::catalog::BlockRange {
                        start: block,
                        end: block + chunked.blocks_per_file - 1,
                    };
                    existing_chunks.insert(StaticFileChunk { segment, range });
                    block += chunked.blocks_per_file;
                }

                if catalog.static_chunks.contains_key(&segment) {
                    carried.insert(key.clone(), ComponentManifest::clone(component));
                }
            }
        }

        let new_chunks: Vec<_> = current_chunks.difference(&existing_chunks).cloned().collect();

        (new_chunks, carried)
    }
}

impl StaticSegment {
    fn from_manifest_key(key: &str) -> Option<Self> {
        match key {
            "headers" => Some(Self::Headers),
            "transactions" => Some(Self::Transactions),
            "receipts" => Some(Self::Receipts),
            "transaction_senders" => Some(Self::TransactionSenders),
            "account_changesets" => Some(Self::AccountChangesets),
            "storage_changesets" => Some(Self::StorageChangesets),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::BlockRange, manifest::ChunkedArchive};

    fn mock_catalog_with_chunks(segments: &[(StaticSegment, &[(u64, u64)])]) -> DatadirCatalog {
        let dir = tempfile::tempdir().unwrap();
        let datadir = dir.path();
        std::fs::create_dir_all(datadir.join("db")).unwrap();
        std::fs::create_dir_all(datadir.join("static_files")).unwrap();

        let mut static_chunks = BTreeMap::new();
        for (segment, ranges) in segments {
            let block_ranges: Vec<BlockRange> =
                ranges.iter().map(|(s, e)| BlockRange { start: *s, end: *e }).collect();

            for range in &block_ranges {
                let filename =
                    format!("static_file_{}_{}_{}", segment.manifest_key(), range.start, range.end);
                std::fs::write(datadir.join("static_files").join(filename), b"data").unwrap();
            }

            static_chunks.insert(*segment, block_ranges);
        }

        DatadirCatalog {
            mdbx_path: datadir.join("db"),
            rocksdb_path: None,
            proofs_path: None,
            static_chunks,
            static_files_root: datadir.join("static_files"),
        }
    }

    #[test]
    fn first_snapshot_uploads_everything() {
        let catalog = mock_catalog_with_chunks(&[
            (StaticSegment::Headers, &[(0, 499_999), (500_000, 999_999)]),
            (StaticSegment::Transactions, &[(0, 499_999)]),
        ]);

        let plan = UploadPlan::from_diff(&catalog, None);

        assert_eq!(plan.new_chunks.len(), 3);
        assert!(plan.carried_segments.is_empty());
        assert!(plan.upload_state);
    }

    #[test]
    fn incremental_snapshot_only_uploads_new_chunks() {
        let catalog = mock_catalog_with_chunks(&[(
            StaticSegment::Headers,
            &[(0, 499_999), (500_000, 999_999)],
        )]);

        let mut prev_components = BTreeMap::new();
        prev_components.insert(
            "headers".to_string(),
            ComponentManifest::Chunked(ChunkedArchive {
                blocks_per_file: 500_000,
                total_blocks: 500_000,
                chunk_sizes: vec![1000],
                chunk_output_files: vec![vec![]],
            }),
        );

        let prev_manifest = SnapshotManifest {
            block: 499_999,
            chain_id: 8453,
            storage_version: 2,
            timestamp: 0,
            base_url: None,
            reth_version: None,
            components: prev_components,
        };

        let plan = UploadPlan::from_diff(&catalog, Some(&prev_manifest));

        assert_eq!(plan.new_chunks.len(), 1);
        assert_eq!(plan.new_chunks[0].range.start, 500_000);
        assert!(plan.carried_segments.contains_key("headers"));
    }
}
