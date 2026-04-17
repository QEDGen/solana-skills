mod spec_bind;
mod verified;

use proc_macro::TokenStream;

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
    // The attribute token stream is a sequence like `verified, hash = "…"`.
    // We only need the leading keyword to dispatch — walk the tokens
    // directly instead of running a full punctuated parser (which would
    // reject multi-token elements like `hash = "…"`).
    let keyword_tok = attr
        .clone()
        .into_iter()
        .find(|t| matches!(t, proc_macro2::TokenTree::Ident(_)));

    let keyword = match keyword_tok {
        Some(proc_macro2::TokenTree::Ident(ref ident)) => ident.to_string(),
        _ => {
            return syn::Error::new_spanned(
                &attr,
                "qed: expected keyword (e.g., `verified`). Usage: #[qed(verified, hash = \"...\")]",
            )
            .to_compile_error();
        }
    };

    match keyword.as_str() {
        // Always route through spec_bind; it falls back to the legacy
        // body-only check when `spec` / `handler` are absent.
        "verified" => spec_bind::expand_bound(attr, item),
        other => {
            let msg = format!("qed: unknown keyword `{}`. Available: `verified`", other);
            let span = match keyword_tok {
                Some(proc_macro2::TokenTree::Ident(i)) => i.span(),
                _ => proc_macro2::Span::call_site(),
            };
            syn::Error::new(span, msg).to_compile_error()
        }
    }
}
