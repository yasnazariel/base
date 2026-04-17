#![doc = include_str!("../README.md")]
#![doc(issue_tracker_base_url = "https://github.com/base/base/issues/")]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod cli;
pub use cli::{CanaryArgs, Cli, HealthArgs, LogArgs, MetricsArgs, ScheduleModeArg};

mod config;
pub use config::{CanaryConfig, ConfigError};

mod scheduler;
pub use scheduler::{ScheduleMode, Scheduler};

mod action;
pub use action::{ActionOutcome, CanaryAction};

mod actions;
pub use actions::{
    BalanceCheckAction, GossipSpamAction, HealthCheckAction, InvalidBatchAction, LoadTestAction,
    LoadTestConfig,
};

mod metrics;
pub use metrics::Metrics;

mod service;
pub use service::CanaryService;
