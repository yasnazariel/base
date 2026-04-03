# `base-common-macros`

Procedural macros for base workspace types.

## Overview

Provides `#[derive(InMemorySize)]` for structs and enums, generating implementations of both
`alloy_consensus::InMemorySize` and (behind `#[cfg(feature = "reth")]`)
`reth_primitives_traits::InMemorySize`.

### Struct fields

By default every field contributes `self.field.size()`. Use `#[in_memory_size(size_of)]` on a
field to use `core::mem::size_of_val(&self.field)` instead, which is appropriate for `Copy`
types such as `Option<u64>` that do not heap-allocate.

```rust,ignore
#[derive(InMemorySize)]
struct Foo<T> {
    data: Vec<T>,
    #[in_memory_size(size_of)]
    count: Option<u64>,
}
```

### Enums

All-unit enums return `core::mem::size_of::<Self>()`. Enums with newtype variants delegate to
`.size()` on the inner value via a `match` expression. Mixed variants (some unit, some newtype)
are supported, where unit arms contribute `0`.

```rust,ignore
#[derive(InMemorySize)]
enum Msg {
    Text(String),
    Empty,
}
```

## Usage

```toml
[dependencies]
base-common-macros = { workspace = true }
```

## License

Licensed under the [MIT License](https://github.com/base/base/blob/main/LICENSE).
