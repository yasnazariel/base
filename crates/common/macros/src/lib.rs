#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod in_memory_size;

/// Derives `alloy_consensus::InMemorySize` and (behind `#[cfg(feature = "reth")]`)
/// `reth_primitives_traits::InMemorySize` for a struct or enum.
///
/// Struct fields call `.size()` by default. Annotate a field with
/// `#[in_memory_size(size_of)]` to use `core::mem::size_of_val` instead.
///
/// All-unit enums return `core::mem::size_of::<Self>()`. Enums with newtype
/// variants dispatch to each variant's `.size()` method. Where-clause bounds
/// (`FieldType: InMemorySize`) are added only for fields whose types contain a
/// generic type parameter; concrete field types are left unbounded and resolved
/// at the call site via normal method resolution.
#[proc_macro_derive(InMemorySize, attributes(in_memory_size))]
pub fn derive_in_memory_size(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    in_memory_size::derive(input).into()
}
