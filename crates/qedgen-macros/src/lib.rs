mod verified;

use proc_macro::TokenStream;
use syn::{parse::Parser, punctuated::Punctuated, Token};

/// Attribute macro for QEDGen verification drift detection.
///
/// # Usage
///
/// Mark a function as verified with a content hash:
/// ```ignore
/// #[qed(verified, hash = "a1b2c3d4e5f67890")]
/// pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
///     // ...
/// }
/// ```
///
/// If the function body or signature changes, compilation fails with an error
/// showing the expected and actual hashes. To set up a new hash, omit it:
/// ```ignore
/// #[qed(verified)]
/// pub fn deposit(...) { ... }
/// // -> compile_error with computed hash
/// ```
#[proc_macro_attribute]
pub fn qed(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr2 = proc_macro2::TokenStream::from(attr);
    let item2 = proc_macro2::TokenStream::from(item);

    let result = dispatch(attr2, item2);
    TokenStream::from(result)
}

fn dispatch(
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    // Parse the first identifier from the attribute to determine the variant
    let parser = Punctuated::<proc_macro2::TokenTree, Token![,]>::parse_terminated;
    let tokens: Vec<proc_macro2::TokenTree> = match parser.parse2(attr.clone()) {
        Ok(punct) => punct.into_iter().collect(),
        Err(e) => return e.to_compile_error(),
    };

    let keyword = match tokens.first() {
        Some(proc_macro2::TokenTree::Ident(ident)) => ident.to_string(),
        _ => {
            return syn::Error::new_spanned(
                &attr,
                "qed: expected keyword (e.g., `verified`). Usage: #[qed(verified, hash = \"...\")]",
            )
            .to_compile_error();
        }
    };

    match keyword.as_str() {
        "verified" => verified::expand(attr, item),
        other => {
            let msg = format!("qed: unknown keyword `{}`. Available: `verified`", other);
            syn::Error::new(
                tokens
                    .first()
                    .map(|t| match t {
                        proc_macro2::TokenTree::Ident(i) => i.span(),
                        _ => proc_macro2::Span::call_site(),
                    })
                    .unwrap_or_else(proc_macro2::Span::call_site),
                msg,
            )
            .to_compile_error()
        }
    }
}
