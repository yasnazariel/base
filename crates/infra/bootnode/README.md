# `base-bootnode`

Standalone EL + CL discv5 bootnode services for the Base network.

## Overview

Base nodes participate in two distinct discv5 networks: an execution-layer (EL)
network identified by an Ethereum fork digest, and a consensus-layer (CL) network
identified by an `opstack` ENR entry encoding the L2 chain ID. Because the two
networks use incompatible ENR identity keys, peer filters, and bootstrap lists,
they cannot share a discv5 instance — but they can be hosted side-by-side in a
single process.

This crate provides:

- [`ElBootnode`] — wraps reth's discv4 + (optional) discv5 services.
- [`ClBootnode`] — wraps [`base_consensus_disc::Discv5Driver`] in
  bootnode-only mode (no ENR forwarding, persistent on-disk store).
- [`Bootnode`] — composes any subset of `{EL, CL}` and runs them under a
  shared [`tokio_util::sync::CancellationToken`].

## Usage

```rust,no_run
use base_bootnode::{Bootnode, ClBootnode, ClBootnodeConfig, ElBootnode, ElBootnodeConfig};
use tokio_util::sync::CancellationToken;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let bootnode = Bootnode::new()
    .with_el(ElBootnode::new(ElBootnodeConfig::default()))
    .with_cl(ClBootnode::new(ClBootnodeConfig::for_chain(8453)));

let cancel = CancellationToken::new();
bootnode.run(cancel).await?;
# Ok(())
# }
```

## License

[MIT License](https://github.com/base/base/blob/main/LICENSE)
