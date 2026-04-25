use proc_macro2::{Span, TokenStream};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::{parse2, ImplItemFn, ItemFn};

/// One of the two function-like syntactic positions `#[qed]` is allowed
/// on: a top-level `fn` (Anchor scaffold + Raydium handlers, Jito
/// inline) or a method inside an `impl` block (Marinade-style
/// `impl Deposit { fn process }`, Squads-style
/// `impl MultisigCreate { fn multisig_create }`). Both share the
/// fields we hash (`attrs`, `sig`, `block`); the macro abstracts over
/// the distinction so callers can pass either through the same path.
pub enum FnLike {
    Item(ItemFn),
    Impl(ImplItemFn),
}

impl FnLike {
    /// Parse the token stream as either an `ItemFn` (free fn) or an
    /// `ImplItemFn` (impl method). Tries free-fn first because it's
    /// the more common shape in user code.
    pub fn from_tokens(item: TokenStream) -> Result<Self, syn::Error> {
        if let Ok(f) = parse2::<ItemFn>(item.clone()) {
            return Ok(FnLike::Item(f));
        }
        parse2::<ImplItemFn>(item).map(FnLike::Impl)
    }

    /// The function's own identifier (`fn <name>`).
    pub fn ident(&self) -> &syn::Ident {
        match self {
            FnLike::Item(f) => &f.sig.ident,
            FnLike::Impl(f) => &f.sig.ident,
        }
    }

    /// Span of the function's name — used for `compile_error!` so the
    /// diagnostic underlines the fn name.
    pub fn name_span(&self) -> Span {
        self.ident().span()
    }

    /// Re-emit the function unchanged. Used when the macro hands back
    /// the original item alongside (or instead of) a `compile_error!`.
    pub fn to_token_stream(&self) -> TokenStream {
        match self {
            FnLike::Item(f) => f.to_token_stream(),
            FnLike::Impl(f) => f.to_token_stream(),
        }
    }

    /// Hash the canonical token stream after stripping every outer
    /// attribute (doc comments, `#[qed(...)]`, `#[inline]`, etc.).
    /// Identical algorithm whether the input was free-fn or
    /// impl-method, modulo each variant's own ToTokens emission. Free-fn
    /// and impl-method differ syntactically in the rare `default fn`
    /// position; otherwise produce identical bytes.
    pub fn content_hash(&self) -> String {
        match self {
            FnLike::Item(f) => {
                let mut stripped = f.clone();
                stripped.attrs.clear();
                sha256_hex16(&stripped.to_token_stream().to_string())
            }
            FnLike::Impl(f) => {
                let mut stripped = f.clone();
                stripped.attrs.clear();
                sha256_hex16(&stripped.to_token_stream().to_string())
            }
        }
    }
}

/// Compute a deterministic content hash for a free `fn`. Kept for
/// backward compatibility with callers that already have an `ItemFn`.
/// New code should prefer `FnLike::content_hash` so impl-methods work
/// out of the box.
#[allow(dead_code)] // public API surface — used by external callers and tests
pub fn content_hash(func: &ItemFn) -> String {
    FnLike::Item(func.clone()).content_hash()
}

/// SHA-256 hash of a string, truncated to 16 hex characters.
pub(crate) fn sha256_hex16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Extract `hash = "..."` from the attribute token stream.
///
/// Expects the form: `verified, hash = "abcdef0123456789"`
fn extract_hash(attr: &TokenStream) -> Result<Option<String>, syn::Error> {
    let tokens: Vec<proc_macro2::TokenTree> = attr.clone().into_iter().collect();

    // Find `hash` `=` `"value"` sequence
    let mut i = 0;
    while i < tokens.len() {
        if let proc_macro2::TokenTree::Ident(ref ident) = tokens[i] {
            if ident == "hash" {
                // Expect `=` next
                if i + 2 < tokens.len() {
                    if let proc_macro2::TokenTree::Punct(ref p) = tokens[i + 1] {
                        if p.as_char() == '=' {
                            if let proc_macro2::TokenTree::Literal(ref lit) = tokens[i + 2] {
                                let lit_str = lit.to_string();
                                let hash = lit_str.trim_matches('"').to_string();
                                if hash.is_empty() {
                                    return Err(syn::Error::new(
                                        lit.span(),
                                        "qed(verified): hash value cannot be empty",
                                    ));
                                }
                                return Ok(Some(hash));
                            }
                        }
                    }
                }
                return Err(syn::Error::new(
                    ident.span(),
                    "qed(verified): expected `hash = \"...\"`",
                ));
            }
        }
        i += 1;
    }

    Ok(None)
}

/// Main expansion for `#[qed(verified, hash = "...")]`.
pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = match FnLike::from_tokens(item.clone()) {
        Ok(f) => f,
        Err(_) => {
            return syn::Error::new_spanned(
                &item,
                "qed(verified): can only be applied to free functions or impl methods",
            )
            .to_compile_error();
        }
    };

    let fn_name = func.ident().to_string();
    let actual_hash = func.content_hash();

    let expected_hash = match extract_hash(&attr) {
        Ok(h) => h,
        Err(e) => return e.to_compile_error(),
    };

    match expected_hash {
        Some(expected) if expected == actual_hash => item,
        Some(expected) => {
            let msg = format!(
                "qed: verified function `{}` has changed since verification \
                 — re-verify or update hash.\n\
                 Expected: {}\n\
                 Actual:   {}",
                fn_name, expected, actual_hash
            );
            let err = syn::Error::new(func.name_span(), msg).to_compile_error();
            let func_tokens = func.to_token_stream();
            quote::quote! {
                #err
                #func_tokens
            }
        }
        None => {
            let msg = format!(
                "qed(verified): no hash provided for `{}`. \
                 Computed hash: {}\n\
                 Usage: #[qed(verified, hash = \"{}\")]",
                fn_name, actual_hash, actual_hash
            );
            let err = syn::Error::new(func.name_span(), msg).to_compile_error();
            let func_tokens = func.to_token_stream();
            quote::quote! {
                #err
                #func_tokens
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn parse_fn(tokens: TokenStream) -> ItemFn {
        syn::parse2(tokens).unwrap()
    }

    #[test]
    fn hash_deterministic() {
        let func = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        });
        let h1 = content_hash(&func);
        let h2 = content_hash(&func);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn hash_ignores_attributes() {
        let with_attr = parse_fn(quote! {
            #[inline(always)]
            #[some_other_attr]
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        });
        let without_attr = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        });
        assert_eq!(content_hash(&with_attr), content_hash(&without_attr));
    }

    #[test]
    fn hash_changes_on_body_change() {
        let v1 = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        });
        let v2 = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount + 2
            }
        });
        assert_ne!(content_hash(&v1), content_hash(&v2));
    }

    #[test]
    fn hash_changes_on_param_type_change() {
        let v1 = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount
            }
        });
        let v2 = parse_fn(quote! {
            pub fn deposit(amount: u128) -> u64 {
                amount
            }
        });
        assert_ne!(content_hash(&v1), content_hash(&v2));
    }

    #[test]
    fn hash_changes_on_return_type_change() {
        let v1 = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount
            }
        });
        let v2 = parse_fn(quote! {
            pub fn deposit(amount: u64) -> u128 {
                amount
            }
        });
        assert_ne!(content_hash(&v1), content_hash(&v2));
    }

    #[test]
    fn hash_changes_on_fn_name_change() {
        let v1 = parse_fn(quote! {
            pub fn deposit(amount: u64) {}
        });
        let v2 = parse_fn(quote! {
            pub fn withdraw(amount: u64) {}
        });
        assert_ne!(content_hash(&v1), content_hash(&v2));
    }

    #[test]
    fn extract_hash_present() {
        let attr = quote! { verified, hash = "abc123def456789a" };
        let result = extract_hash(&attr).unwrap();
        assert_eq!(result, Some("abc123def456789a".to_string()));
    }

    #[test]
    fn extract_hash_absent() {
        let attr = quote! { verified };
        let result = extract_hash(&attr).unwrap();
        assert_eq!(result, None);
    }

    /// `FnLike` accepts impl-method-shaped input (with `&mut self`).
    /// In syn 2.0, `ItemFn` is lenient and parses receivers, so we
    /// pick `Item` first by design — both variants hash the same
    /// bytes for the same input. The drift fixture exercises the
    /// real impl-method path end-to-end.
    #[test]
    fn fn_like_handles_method_shape_input() {
        let tokens = quote! {
            pub fn process(&mut self, lamports: u64) -> Result<()> {
                self.state.total_lamports += lamports;
                Ok(())
            }
        };
        let func = FnLike::from_tokens(tokens).unwrap();
        let h = func.content_hash();
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The only syntactic shape that REQUIRES the ImplItemFn fallback
    /// is `default fn` (a stable feature in trait impls). Free `fn`
    /// rejects `default`, so this forces the second-try branch.
    #[test]
    fn fn_like_falls_back_to_impl_for_default_fn() {
        let tokens = quote! {
            default fn process(&mut self) -> Result<()> { Ok(()) }
        };
        let func = FnLike::from_tokens(tokens).unwrap();
        assert!(matches!(func, FnLike::Impl(_)));
        assert_eq!(func.content_hash().len(), 16);
    }

    #[test]
    fn fn_like_prefers_item_fn_when_both_parse() {
        // A function with no `self` parameter parses cleanly as
        // ItemFn; we should pick that branch first since it's the
        // more common shape.
        let tokens = quote! {
            pub fn deposit(amount: u64) -> u64 { amount + 1 }
        };
        let func = FnLike::from_tokens(tokens).unwrap();
        assert!(matches!(func, FnLike::Item(_)));
    }

    #[test]
    fn fn_like_impl_hash_changes_on_body_edit() {
        let v1 = FnLike::from_tokens(quote! {
            pub fn process(&mut self, x: u64) -> Result<()> {
                self.x += x; Ok(())
            }
        })
        .unwrap();
        let v2 = FnLike::from_tokens(quote! {
            pub fn process(&mut self, x: u64) -> Result<()> {
                self.x += x + 1; Ok(())
            }
        })
        .unwrap();
        assert_ne!(v1.content_hash(), v2.content_hash());
    }
}
