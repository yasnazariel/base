# Virtual Address Registry Example

End-to-end demonstration of TIP-1022 virtual address resolution on the Base
stack. Shows how to implement a stateful precompile that resolves virtual
deposit addresses to registered master wallets, eliminating sweep transactions.

## What This Demonstrates

1. **Custom `EvmOverride`** — Injects a registry precompile into the test
   harness EVM, intercepting calls to the registry address with full storage
   access.

2. **Stateful precompile** — The registry stores `masterId → masterAddress`
   mappings in EVM storage slots, supporting registration (with 32-bit
   proof-of-work) and resolution.

3. **Full e2e flow** — From RPC-style transaction submission through EVM
   execution to verified balance changes, batched and derived through the
   consensus pipeline.

## Running

```sh
cargo test -p virtual-address-registry
```
