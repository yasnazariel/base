# base-address-resolution

Types and traits for virtual address resolution, enabling deposit forwarding
to master wallets without sweep transactions or state bloat.

## Overview

This crate provides the building blocks for virtual address schemes where a
specially formatted 20-byte address encodes a lookup key (`masterId`) and a
per-user tag (`userTag`). A registry maps each `masterId` to a master address.
Token contracts can call the registry to resolve a virtual recipient to its
master before crediting, eliminating per-user sweep transactions.

## Address Format

A virtual address is 20 bytes laid out as:

```text
[4-byte masterId] [10-byte VIRTUAL_MAGIC] [6-byte userTag]
```

The 10-byte magic `0xFDFDFDFDFDFDFDFDFDFD` in the middle distinguishes
virtual addresses from regular addresses.

## Usage

```rust,ignore
use base_address_resolution::{VirtualAddress, MasterId, UserTag};

let addr: Address = /* ... */;
if VirtualAddress::is_virtual(addr) {
    let (master_id, user_tag) = VirtualAddress::decode(addr).unwrap();
    // look up master_id in a registry to get the effective recipient
}
```
