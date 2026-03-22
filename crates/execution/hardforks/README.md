# `base-execution-forks`

Hardfork definitions for Base.

## Overview

Provides execution-layer hardfork utilities by re-exporting and extending the base hardfork
definitions from `base-alloy-upgrades`. Exposes the pre-configured hardfork activation schedule
for Base Mainnet (`BASE_MAINNET_HARDFORKS`), along with `BaseChainUpgradesExt` for converting
any `BaseChainUpgrades` into a full `ChainHardforks`.

## Usage

Add the dependency to your `Cargo.toml`:

```toml
[dependencies]
base-execution-forks = { workspace = true }
```

```rust,ignore
use base_execution_forks::{BASE_MAINNET_HARDFORKS, BaseUpgrades};

let forks = BASE_MAINNET_HARDFORKS.clone();
let is_jovian = forks.is_jovian_active_at_timestamp(timestamp);
```

## License

Licensed under the [MIT License](https://github.com/base/base/blob/main/LICENSE).
