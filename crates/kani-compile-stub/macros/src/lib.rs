//! Proc-macro half of the kani compile stub.
//!
//! Each attribute keeps the item unchanged so the harness body is fully
//! type-checked by ordinary rustc. None of this is executable — the stub
//! exists only so `cargo rustc --test kani -- --cfg kani` can compile a
//! generated `tests/kani.rs` without the Kani toolchain.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// `#[kani::proof]` — pass-through. `allow(dead_code)` because harness
/// fns are never called in the compile-gate build.
#[proc_macro_attribute]
pub fn proof(_args: TokenStream, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);
    quote! {
        #[allow(dead_code)]
        #item
    }
    .into()
}

/// `#[kani::unwind(N)]` — pass-through, argument ignored.
#[proc_macro_attribute]
pub fn unwind(_args: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// `#[kani::solver(...)]` — pass-through, argument ignored.
#[proc_macro_attribute]
pub fn solver(_args: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// `#[derive(kani::Arbitrary)]` — emits an `unimplemented!()` impl of the
/// stub's `Arbitrary` trait. Weaker than the real derive (no per-field
/// `Arbitrary` bounds), which is fine for a compile gate: the bug surface
/// is the harness bodies, and real `cargo kani` still checks the derive.
#[proc_macro_derive(Arbitrary)]
pub fn derive_arbitrary(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        impl #impl_generics ::kani::Arbitrary for #name #ty_generics #where_clause {
            fn any() -> Self {
                unimplemented!("kani compile stub is not executable")
            }
        }
    }
    .into()
}
