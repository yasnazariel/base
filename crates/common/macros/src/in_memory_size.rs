//! Derive macro implementation for `InMemorySize`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericParam, Ident, Type, parse_quote};

/// Derive `alloy_consensus::InMemorySize` (and optionally
/// `reth_primitives_traits::InMemorySize`) for a struct or enum.
pub(crate) fn derive(input: DeriveInput) -> TokenStream {
    let name = &input.ident;
    let (impl_generics, ty_generics, _) = input.generics.split_for_impl();

    let generic_params: Vec<Ident> = input
        .generics
        .params
        .iter()
        .filter_map(|p| if let GenericParam::Type(t) = p { Some(t.ident.clone()) } else { None })
        .collect();

    let existing: Vec<syn::WherePredicate> = input
        .generics
        .where_clause
        .as_ref()
        .map(|w| w.predicates.iter().cloned().collect())
        .unwrap_or_default();

    let alloy_path: TokenStream = quote! { alloy_consensus::InMemorySize };
    let reth_path: TokenStream = quote! { reth_primitives_traits::InMemorySize };

    let alloy_body = size_body(&input.data, &alloy_path);
    let reth_body = size_body(&input.data, &reth_path);

    let alloy_bounds = bounds(&input.data, &alloy_path, &generic_params);
    let reth_bounds = bounds(&input.data, &reth_path, &generic_params);

    let alloy_where = where_clause(&existing, &alloy_bounds);
    let reth_where = where_clause(&existing, &reth_bounds);

    quote! {
        impl #impl_generics alloy_consensus::InMemorySize for #name #ty_generics #alloy_where {
            #[inline]
            fn size(&self) -> usize {
                #alloy_body
            }
        }

        #[cfg(feature = "reth")]
        impl #impl_generics reth_primitives_traits::InMemorySize for #name #ty_generics #reth_where {
            #[inline]
            fn size(&self) -> usize {
                #reth_body
            }
        }
    }
}

fn where_clause(existing: &[syn::WherePredicate], extra: &[syn::WherePredicate]) -> TokenStream {
    if existing.is_empty() && extra.is_empty() {
        return quote! {};
    }
    quote! { where #(#existing,)* #(#extra,)* }
}

fn has_size_of_attr(field: &syn::Field) -> bool {
    field.attrs.iter().any(|attr| {
        attr.path().is_ident("in_memory_size")
            && attr.parse_args::<Ident>().map(|id| id == "size_of").unwrap_or(false)
    })
}

/// Returns `true` if `ty` contains any of the listed type-parameter identifiers.
fn contains_generic(ty: &Type, params: &[Ident]) -> bool {
    match ty {
        Type::Path(tp) => {
            for seg in &tp.path.segments {
                if params.contains(&seg.ident) {
                    return true;
                }
                if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                    for arg in &ab.args {
                        if let syn::GenericArgument::Type(inner) = arg
                            && contains_generic(inner, params) {
                                return true;
                            }
                    }
                }
            }
            false
        }
        Type::Reference(r) => contains_generic(&r.elem, params),
        Type::Tuple(t) => t.elems.iter().any(|e| contains_generic(e, params)),
        _ => false,
    }
}

/// Generates the body of `fn size(&self) -> usize` using fully-qualified
/// `<_ as trait_path>::size(x)` calls to avoid ambiguity when multiple
/// `InMemorySize` traits with identical method names are in scope.
fn size_body(data: &Data, trait_path: &TokenStream) -> TokenStream {
    match data {
        Data::Enum(e) => {
            let all_unit = e.variants.iter().all(|v| matches!(v.fields, Fields::Unit));
            if all_unit {
                return quote! { core::mem::size_of::<Self>() };
            }
            let arms: Vec<TokenStream> = e
                .variants
                .iter()
                .map(|v| {
                    let ident = &v.ident;
                    match &v.fields {
                        Fields::Unnamed(f) if f.unnamed.len() == 1 => {
                            quote! { Self::#ident(x) => <_ as #trait_path>::size(x) }
                        }
                        Fields::Unit => quote! { Self::#ident => 0 },
                        _ => panic!(
                            "#[derive(InMemorySize)]: only unit or single-field tuple variants are supported"
                        ),
                    }
                })
                .collect();
            quote! { match self { #(#arms,)* } }
        }
        Data::Struct(s) => {
            let field_exprs: Vec<TokenStream> = s
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let acc = f.ident.as_ref().map(|n| quote! { #n }).unwrap_or_else(|| {
                        let idx = syn::Index::from(i);
                        quote! { #idx }
                    });
                    if has_size_of_attr(f) {
                        quote! { core::mem::size_of_val(&self.#acc) }
                    } else {
                        quote! { <_ as #trait_path>::size(&self.#acc) }
                    }
                })
                .collect();
            if field_exprs.is_empty() {
                quote! { 0 }
            } else {
                quote! { #(#field_exprs)+* }
            }
        }
        Data::Union(_) => panic!("#[derive(InMemorySize)] is not supported for unions"),
    }
}

/// Generates where-clause predicates: only for fields/variants whose types
/// contain a generic parameter, to avoid emitting unsatisfied bounds on
/// concrete types (e.g. `Sealed<TxDeposit>`) that are resolved via deref.
fn bounds(data: &Data, trait_path: &TokenStream, params: &[Ident]) -> Vec<syn::WherePredicate> {
    match data {
        Data::Enum(e) => e
            .variants
            .iter()
            .filter_map(|v| {
                if let Fields::Unnamed(f) = &v.fields
                    && f.unnamed.len() == 1 {
                        let ty = &f.unnamed[0].ty;
                        if contains_generic(ty, params) {
                            return Some(parse_quote!(#ty: #trait_path));
                        }
                    }
                None
            })
            .collect(),
        Data::Struct(s) => s
            .fields
            .iter()
            .filter(|f| !has_size_of_attr(f) && contains_generic(&f.ty, params))
            .map(|f| {
                let ty = &f.ty;
                parse_quote!(#ty: #trait_path)
            })
            .collect(),
        Data::Union(_) => vec![],
    }
}
