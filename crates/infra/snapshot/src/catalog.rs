//! Datadir catalog — walks a reth datadir and classifies files for snapshotting.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use eyre::{Result, bail};
use tracing::debug;

/// A segment type within reth's static files directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StaticSegment {
    /// Block headers (required by all tiers).
    Headers,
    /// Raw transactions.
    Transactions,
    /// Transaction receipts.
    Receipts,
    /// Pre-computed transaction senders.
    TransactionSenders,
    /// Account-level state change sets (archive tier).
    AccountChangesets,
    /// Storage-level state change sets (archive tier).
    StorageChangesets,
}

impl StaticSegment {
    /// Returns the reth manifest key for this segment (matches `SnapshotComponentType` keys).
    pub const fn manifest_key(&self) -> &'static str {
        match self {
            Self::Headers => "headers",
            Self::Transactions => "transactions",
            Self::Receipts => "receipts",
            Self::TransactionSenders => "transaction_senders",
            Self::AccountChangesets => "account_changesets",
            Self::StorageChangesets => "storage_changesets",
        }
    }

    /// Parses a segment from reth's static file directory prefix.
    pub fn from_dir_prefix(s: &str) -> Option<Self> {
        match s {
            "headers" => Some(Self::Headers),
            "transactions" => Some(Self::Transactions),
            "receipts" => Some(Self::Receipts),
            "transaction_senders" | "transaction-senders" => Some(Self::TransactionSenders),
            "account_changesets" | "account-change-sets" => Some(Self::AccountChangesets),
            "storage_changesets" | "storage-change-sets" => Some(Self::StorageChangesets),
            _ => None,
        }
    }
}

impl fmt::Display for StaticSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.manifest_key())
    }
}

/// A block range covered by a single static file chunk (e.g., 0..499999).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockRange {
    /// First block in the range (inclusive).
    pub start: u64,
    /// Last block in the range (inclusive).
    pub end: u64,
}

impl fmt::Display for BlockRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

/// A single static file chunk identified by segment type and block range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StaticFileChunk {
    /// Which static file segment this chunk belongs to.
    pub segment: StaticSegment,
    /// Block range covered by this chunk.
    pub range: BlockRange,
}

impl StaticFileChunk {
    /// Returns the archive filename matching reth's convention: `{segment}-{start}-{end}.tar.zst`.
    pub fn archive_name(&self) -> String {
        format!("{}-{}-{}.tar.zst", self.segment.manifest_key(), self.range.start, self.range.end)
    }
}

/// Complete catalog of a reth datadir ready for snapshotting.
#[derive(Debug)]
pub struct DatadirCatalog {
    /// Path to the `db/` directory (MDBX).
    pub mdbx_path: PathBuf,
    /// Path to the `rocksdb/` directory (optional, storage v2 only).
    pub rocksdb_path: Option<PathBuf>,
    /// Path to the proofs extension MDBX (separate from reth datadir).
    pub proofs_path: Option<PathBuf>,
    /// Static file chunks found, grouped by segment.
    pub static_chunks: BTreeMap<StaticSegment, Vec<BlockRange>>,
    /// Root path of the `static_files/` directory.
    pub static_files_root: PathBuf,
}

impl DatadirCatalog {
    /// Walk a reth datadir and proofs path, building a complete catalog.
    pub fn from_paths(datadir: &Path, proofs_path: Option<&Path>) -> Result<Self> {
        let mdbx_path = datadir.join("db");
        if !mdbx_path.exists() {
            bail!("MDBX directory not found at {}", mdbx_path.display());
        }

        let rocksdb_path = {
            let p = datadir.join("rocksdb");
            if p.exists() { Some(p) } else { None }
        };

        let proofs_path =
            proofs_path.and_then(|p| if p.exists() { Some(p.to_path_buf()) } else { None });

        let static_files_root = datadir.join("static_files");
        let static_chunks = if static_files_root.exists() {
            Self::scan_static_files(&static_files_root)?
        } else {
            BTreeMap::new()
        };

        debug!(
            mdbx = %mdbx_path.display(),
            rocksdb = ?rocksdb_path.as_ref().map(|p| p.display().to_string()),
            proofs = ?proofs_path.as_ref().map(|p| p.display().to_string()),
            static_segments = static_chunks.len(),
            "cataloged datadir",
        );

        Ok(Self { mdbx_path, rocksdb_path, proofs_path, static_chunks, static_files_root })
    }

    /// Returns a flat list of all static file chunks.
    pub fn all_static_chunks(&self) -> Vec<StaticFileChunk> {
        self.static_chunks
            .iter()
            .flat_map(|(segment, ranges)| {
                ranges.iter().map(|range| StaticFileChunk { segment: *segment, range: *range })
            })
            .collect()
    }

    /// Returns source file paths for a given static file chunk.
    ///
    /// Reth names static files as `static_file_{segment}_{start}_{end}[_suffix]`.
    /// A single chunk may consist of multiple files (e.g., data + offsets + config).
    pub fn source_files_for_chunk(&self, chunk: &StaticFileChunk) -> Result<Vec<PathBuf>> {
        let prefix = format!(
            "static_file_{}_{}_{}",
            chunk.segment.manifest_key(),
            chunk.range.start,
            chunk.range.end
        );

        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.static_files_root)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with(&prefix) {
                    files.push(entry.path());
                }
        }

        if files.is_empty() {
            bail!(
                "no source files found for chunk {} in {}",
                chunk.archive_name(),
                self.static_files_root.display(),
            );
        }

        files.sort();
        Ok(files)
    }

    fn scan_static_files(static_dir: &Path) -> Result<BTreeMap<StaticSegment, Vec<BlockRange>>> {
        let mut chunks: BTreeMap<StaticSegment, Vec<BlockRange>> = BTreeMap::new();
        let mut seen: std::collections::HashSet<(StaticSegment, BlockRange)> =
            std::collections::HashSet::new();

        for entry in std::fs::read_dir(static_dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };

            if let Some(parsed) = Self::parse_static_filename(&name)
                && seen.insert((parsed.0, parsed.1)) {
                    chunks.entry(parsed.0).or_default().push(parsed.1);
                }
        }

        for ranges in chunks.values_mut() {
            ranges.sort();
        }

        Ok(chunks)
    }

    /// Parse a filename like `static_file_headers_0_499999_none_lz4` into segment + range.
    fn parse_static_filename(name: &str) -> Option<(StaticSegment, BlockRange)> {
        let name = name.strip_prefix("static_file_")?;

        let segments_to_try = [
            ("transaction_senders_", StaticSegment::TransactionSenders),
            ("account_changesets_", StaticSegment::AccountChangesets),
            ("storage_changesets_", StaticSegment::StorageChangesets),
            ("transactions_", StaticSegment::Transactions),
            ("receipts_", StaticSegment::Receipts),
            ("headers_", StaticSegment::Headers),
        ];

        for (prefix, segment) in segments_to_try {
            if let Some(rest) = name.strip_prefix(prefix) {
                let mut parts = rest.splitn(3, '_');
                let start: u64 = parts.next()?.parse().ok()?;
                let end: u64 = parts.next()?.parse().ok()?;
                return Some((segment, BlockRange { start, end }));
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_static_filenames() {
        let cases = [
            (
                "static_file_headers_0_499999_none_lz4",
                Some((StaticSegment::Headers, BlockRange { start: 0, end: 499999 })),
            ),
            (
                "static_file_transactions_500000_999999_none_lz4",
                Some((StaticSegment::Transactions, BlockRange { start: 500000, end: 999999 })),
            ),
            (
                "static_file_transaction_senders_0_499999_none_lz4",
                Some((StaticSegment::TransactionSenders, BlockRange { start: 0, end: 499999 })),
            ),
            (
                "static_file_account_changesets_0_499999_none_lz4",
                Some((StaticSegment::AccountChangesets, BlockRange { start: 0, end: 499999 })),
            ),
            ("not_a_static_file", None),
        ];

        for (input, expected) in cases {
            let result = DatadirCatalog::parse_static_filename(input);
            assert_eq!(result, expected, "failed for input: {input}");
        }
    }

    #[test]
    fn catalog_from_paths() {
        let dir = tempfile::tempdir().unwrap();
        let datadir = dir.path();

        std::fs::create_dir_all(datadir.join("db")).unwrap();
        std::fs::create_dir_all(datadir.join("static_files")).unwrap();

        std::fs::write(datadir.join("static_files/static_file_headers_0_499999_none_lz4"), b"data")
            .unwrap();
        std::fs::write(
            datadir.join("static_files/static_file_headers_500000_999999_none_lz4"),
            b"data",
        )
        .unwrap();
        std::fs::write(
            datadir.join("static_files/static_file_transactions_0_499999_none_lz4"),
            b"data",
        )
        .unwrap();

        let catalog = DatadirCatalog::from_paths(datadir, None).unwrap();

        assert_eq!(catalog.static_chunks.len(), 2);
        assert_eq!(catalog.static_chunks[&StaticSegment::Headers].len(), 2);
        assert_eq!(catalog.static_chunks[&StaticSegment::Transactions].len(), 1);
        assert!(catalog.rocksdb_path.is_none());
        assert!(catalog.proofs_path.is_none());
    }

    #[test]
    fn source_files_for_chunk_collects_all_related_files() {
        let dir = tempfile::tempdir().unwrap();
        let datadir = dir.path();

        std::fs::create_dir_all(datadir.join("db")).unwrap();
        std::fs::create_dir_all(datadir.join("static_files")).unwrap();

        std::fs::write(
            datadir.join("static_files/static_file_headers_0_499999_none_lz4"),
            b"data1",
        )
        .unwrap();
        std::fs::write(
            datadir.join("static_files/static_file_headers_0_499999_none_lz4.off"),
            b"offsets",
        )
        .unwrap();

        let catalog = DatadirCatalog::from_paths(datadir, None).unwrap();
        let chunk = StaticFileChunk {
            segment: StaticSegment::Headers,
            range: BlockRange { start: 0, end: 499999 },
        };

        let files = catalog.source_files_for_chunk(&chunk).unwrap();
        assert_eq!(files.len(), 2);
    }
}
