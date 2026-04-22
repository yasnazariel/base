#![doc = include_str!("../README.md")]

/// Boot info types exposed by the stub crate.
pub mod boot;
pub use boot::BootInfoStruct;

/// Client constants exposed by the stub crate.
pub mod client;
pub use client::DEFAULT_INTERMEDIATE_ROOT_INTERVAL;
