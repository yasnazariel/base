#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod catalog;
pub use catalog::{BlockRange, DatadirCatalog, StaticFileChunk, StaticSegment};

mod compress;
pub use compress::{CompressedArchive, Compressor};

mod config;
pub use config::SnapshotConfig;

mod diff;
pub use diff::UploadPlan;

mod docker;
pub use docker::DockerClient;

mod manifest;
pub use manifest::{
    ChunkedArchive, ComponentManifest, ManifestBuilder, OutputFileChecksum, SingleArchive,
    SnapshotManifest,
};

mod metrics;
pub use metrics::Metrics;

mod orchestrator;
pub use orchestrator::SnapshotOrchestrator;

mod storage;
pub use storage::SnapshotStorage;
