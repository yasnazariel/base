#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod config;
pub use config::BatcherConfig;

mod recent_txs;
pub use recent_txs::{MAX_CHECK_RECENT_TXS_DEPTH, RecentTxScanner, SCAN_FETCH_CONCURRENCY};

mod endpoint_pool;
pub use endpoint_pool::{EndpointPool, HealthMonitor, Probe};

mod source;
pub use source::{L2EndpointPool, RpcPollingSource};

mod subscription;
pub use subscription::{NullSubscription, WsBlockSubscription};

mod l1_source;
pub use l1_source::{
    L1EndpointPool, NullL1HeadSubscription, RpcL1HeadPollingSource, WsL1HeadSubscription,
};

mod throttle;
pub use throttle::{EndpointRole, RpcThrottleClient};

mod safe_head_poller;
pub use safe_head_poller::{RollupEndpointPool, SafeHeadPoller, SafeHeadProvider};

mod service;
pub use service::{BatcherService, ReadyBatcher};
