#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod error;
pub use error::{DriverError, LeadershipError};

mod status;
pub use status::{LeaderStatus, LeaderStatusReceiver, LeaderStatusSender};

mod validator;
pub use validator::{ClusterMembership, ValidatorEntry, ValidatorId};

mod config;
pub use config::{
    HealthThresholds, LeadershipConfig, LeadershipMode, RaftTimeouts, TransportConfig,
};

mod health;
pub use health::{HealthAggregator, HealthFailure, HealthSignals, HealthVerdict};

mod admin;
pub use admin::{LeadershipCommand, LeadershipCommandReceiver, LeadershipCommandSender};

mod driver;
pub use driver::{ConsensusDriver, DriverContext, DriverEvent, DriverRequest, DriverRequestKind};

mod openraft_driver;
pub use openraft_driver::{
    AppliedState, BootstrapTables, Codec, Frame, LOG_TREE, LeadershipRaft, META_KEY_APPLIED,
    META_KEY_LAST_PURGED, META_KEY_SNAPSHOT, META_KEY_VOTE, META_TREE, MetricsTranslator, NodeId,
    NodeIdHash, OpenraftDriver, PauseFlag, RaftServer, RaftWireRequest, RaftWireResponse,
    RequestHandler, SledLogReader, SledLogStore, SledSnapshotBuilder, SledStateMachine, StorageErr,
    StoredSnapshot, TcpRaftNetwork, TcpRaftNetworkFactory, TypeConfig,
};

#[cfg(any(test, feature = "test-utils"))]
mod mock_driver;
#[cfg(any(test, feature = "test-utils"))]
pub use mock_driver::{MockDriver, MockState};

mod actor;
pub use actor::{
    CHANNEL_CAPACITY, HealthSignalUpdate, LeadershipActor, LeadershipHandles, RunningActor,
};
