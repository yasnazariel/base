use alloy_primitives::{Address, address, b256};

/// Gas per transaction not creating a contract.
pub const MIN_TRANSACTION_GAS: u64 = 21_000u64;

/// Mainnet prune delete limit.
pub const MAINNET_PRUNE_DELETE_LIMIT: usize = 20000;

/// `DepositEvent` topic used when constructing deposit contract metadata from custom genesis files.
#[allow(dead_code)]
pub(crate) const DEPOSIT_CONTRACT_TOPIC: alloy_primitives::B256 =
    b256!("0x649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5");

/// The Base `L2ToL1MessagePasser` predeploy used to derive the Isthmus genesis withdrawals root.
pub(crate) const L2_TO_L1_MESSAGE_PASSER: Address =
    address!("0x4200000000000000000000000000000000000016");
