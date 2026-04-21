#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod error;
pub use error::{BootnodeError, BootnodeResult};

mod runner;
pub use runner::{Bootnode, BootnodeSide};

mod el;
pub use el::{DEFAULT_EL_BOOTNODE_PORT, ElBootnode, ElBootnodeConfig, ElKeyLoader};

mod cl;
pub use cl::{ClBootnode, ClBootnodeConfig, ClKeyLoader, DEFAULT_CL_BOOTNODE_PORT};
