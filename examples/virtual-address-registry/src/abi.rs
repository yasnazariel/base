//! ABI definitions for the virtual address registry precompile.

use alloy_sol_types::sol;

sol! {
    /// Interface for the virtual address registry.
    #[derive(Debug)]
    interface IAddressRegistry {
        /// Register the caller as a virtual-address master.
        /// The `salt` must satisfy the 32-bit proof-of-work requirement.
        function registerVirtualMaster(bytes32 salt) external returns (bytes4 masterId);

        /// Return the master address for a given master ID.
        function getMaster(bytes4 masterId) external view returns (address);

        /// Resolve an address: if virtual, return the registered master; otherwise return the input.
        function resolveRecipient(address to) external view returns (address);

        /// Check whether an address matches the virtual address format.
        function isVirtualAddress(address addr) external pure returns (bool);

        event MasterRegistered(bytes4 indexed masterId, address indexed masterAddress);

        error MasterIdCollision(address existing);
        error InvalidMasterAddress();
        error ProofOfWorkFailed();
        error VirtualAddressUnregistered();
    }
}
