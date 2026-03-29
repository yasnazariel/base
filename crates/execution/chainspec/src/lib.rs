#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod basefee;
pub use basefee::*;

mod builder;
pub use builder::OpChainSpecBuilder;

mod spec;
pub use spec::{
    BASE_DEV, BASE_DEVNET_0_SEPOLIA_DEV_0, BASE_MAINNET, BASE_SEPOLIA, BASE_ZERONET,
    OpChainSpec, OpGenesisInfo, SUPPORTED_CHAINS,
};
