#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

mod types;
pub use types::{MasterId, REGISTRY_ADDRESS, RegistryError, UserTag, VIRTUAL_MAGIC};

mod virtual_addr;
pub use virtual_addr::VirtualAddress;

mod traits;
pub use traits::{AddressResolver, MasterRegistry};
