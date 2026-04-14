use proc_macro2::TokenStream;
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::{parse2, ItemFn};

/// Compute a deterministic content hash for a function.
///
/// Strips all attributes and doc comments, normalizes via syn round-trip,
/// then SHA-256 hashes the result, truncated to 16 hex chars.
pub fn content_hash(func: &ItemFn) -> String {
    let mut stripped = func.clone();
    // Remove all attributes (including doc comments, #[qed(...)], #[inline], etc.)
    stripped.attrs.clear();
    // Normalize via ToTokens -> String (deterministic, whitespace-insensitive)
    let canonical = stripped.to_token_stream().to_string();
    sha256_hex16(&canonical)
}

/// SHA-256 hash of a string, truncated to 16 hex characters.
fn sha256_hex16(input: &str) -> String {
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
                                // Parse the string literal
                                let lit_str = lit.to_string();
                                // Strip surrounding quotes
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
    // Parse the item as a function
    let func: ItemFn = match parse2(item.clone()) {
        Ok(f) => f,
        Err(_) => {
            return syn::Error::new_spanned(
                &item,
                "qed(verified): can only be applied to functions",
            )
            .to_compile_error();
        }
    };

    let fn_name = func.sig.ident.to_string();

    // Compute the content hash
    let actual_hash = content_hash(&func);

    // Extract expected hash from attribute
    let expected_hash = match extract_hash(&attr) {
        Ok(h) => h,
        Err(e) => return e.to_compile_error(),
    };

    match expected_hash {
        Some(expected) if expected == actual_hash => {
            // Hash matches — pass through unchanged
            item
        }
        Some(expected) => {
            // Hash mismatch — drift detected
            let msg = format!(
                "qed: verified function `{}` has changed since verification \
                 — re-verify or update hash.\n\
                 Expected: {}\n\
                 Actual:   {}",
                fn_name, expected, actual_hash
            );
            // Emit compile_error AND the original function (so other errors don't cascade)
            let err = syn::Error::new(func.sig.ident.span(), msg).to_compile_error();
            quote::quote! {
                #err
                #func
            }
        }
        None => {
            // No hash provided — setup mode
            let msg = format!(
                "qed(verified): no hash provided for `{}`. \
                 Computed hash: {}\n\
                 Usage: #[qed(verified, hash = \"{}\")]",
                fn_name, actual_hash, actual_hash
            );
            let err = syn::Error::new(func.sig.ident.span(), msg).to_compile_error();
            quote::quote! {
                #err
                #func
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
}
