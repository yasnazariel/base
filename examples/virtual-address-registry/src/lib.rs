#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod abi;
pub use abi::IAddressRegistry;

mod registry;
pub use registry::StorageBackedRegistry;

mod evm_override;
pub use evm_override::RegistryEvmOverride;

/// Compiled EVM bytecode for the test contracts.
pub mod contracts;
