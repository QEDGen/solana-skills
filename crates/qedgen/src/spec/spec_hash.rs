//! Handler-block extraction + SHA-256-hex16 spec/body hashing.
//!
//! The canonical algorithms live in the shared `qedgen-hash-core` crate,
//! which `qedgen-macros` also depends on — so the CLI and the proc-macro's
//! compile-time recomputation of `hash = "..."` / `spec_hash = "..."` agree
//! by construction (they used to be hand-kept mirrors; that drift class bit
//! once via `--update-hashes`). This module keeps the syn-dependent wrappers
//! (`body_hash_for_*`, `accounts_struct_hash`) and re-exports the rest so
//! `crate::spec_hash::…` call sites are unchanged.

use quote::ToTokens;

// `spec_hash_for_handler` has call sites across drift/reconcile/fill/
// scaffold; the other three are part of this module's historical API and
// are exercised by the test module below (qedgen is a binary crate, so
// rustc flags re-exports without non-test callers — hence the allow).
use qedgen_hash_core::token_stream_hash;
#[allow(unused_imports)]
pub use qedgen_hash_core::{
    extract_handler_block, normalize_spec_block, spec_context_digest, spec_hash_for_handler,
};

/// Body hash of a `syn::ItemFn`: strip all outer attributes, then hash the
/// canonical token stream. Same composition as
/// `qedgen-macros::verified::FnLike::content_hash` — both delegate to
/// `qedgen_hash_core::token_stream_hash`.
pub fn body_hash_for_fn(func: &syn::ItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    token_stream_hash(&stripped.to_token_stream())
}

/// Body hash for an impl method; same algorithm as `body_hash_for_fn`
/// (method-shape `#[qed]` annotations).
pub fn body_hash_for_impl_fn(func: &syn::ImplItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    token_stream_hash(&stripped.to_token_stream())
}

/// Hash a `pub struct <name>` from Rust source. Same syn walk as
/// `qedgen-macros::spec_bind::accounts_struct_hash_in`; the hash itself is
/// the shared `token_stream_hash`. Seals the handler's
/// `#[derive(Accounts)]` struct so constraint edits (`#[account(mut)]`,
/// `has_one`, `seeds`) trip `compile_error!` like body edits do.
///
/// Walks top-level items, then descends into inline `pub mod` blocks;
/// first match wins. `None` if the source isn't valid Rust or the struct
/// doesn't exist.
pub fn accounts_struct_hash(source: &str, struct_name: &str) -> Option<String> {
    let file: syn::File = syn::parse_str(source).ok()?;
    accounts_struct_hash_in_items(&file.items, struct_name)
}

fn accounts_struct_hash_in_items(items: &[syn::Item], struct_name: &str) -> Option<String> {
    for item in items {
        match item {
            syn::Item::Struct(s) if s.ident == struct_name => {
                let mut stripped = s.clone();
                stripped.attrs.clear();
                // Same canonicalization as `body_hash_for_fn` so this agrees
                // byte-for-byte with the proc-macro regardless of tokenization
                // (rustc vs from_str). Raw `to_token_stream().to_string()`
                // carries per-`Punct` `Spacing` from source spacing — hidden
                // drift between the binary and the macro on the same file.
                return Some(token_stream_hash(&stripped.to_token_stream()));
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, sub_items)) = &item_mod.content {
                    if let Some(h) = accounts_struct_hash_in_items(sub_items, struct_name) {
                        return Some(h);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
spec Demo

handler foo (x : U64) : State.A -> State.A {
  requires state.count + x <= 100
  effect { count += x }
}

handler bar : State.A -> State.B {
  effect { /* transition */ }
}
"#;

    #[test]
    fn extract_foo() {
        let block = extract_handler_block(SAMPLE, "foo").unwrap();
        assert!(block.starts_with('{'));
        assert!(block.ends_with('}'));
        assert!(block.contains("count += x"));
        assert!(!block.contains("bar"));
    }

    #[test]
    fn hash_stable_and_differs() {
        let h1 = spec_hash_for_handler(SAMPLE, "foo").unwrap();
        let h2 = spec_hash_for_handler(SAMPLE, "foo").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        let h_bar = spec_hash_for_handler(SAMPLE, "bar").unwrap();
        assert_ne!(h1, h_bar);
    }

    #[test]
    fn missing_handler_is_none() {
        assert!(spec_hash_for_handler(SAMPLE, "nonexistent").is_none());
    }

    /// Top-level changes (consts, types, etc.) outside any handler block
    /// must invalidate every handler's spec_hash, even when the handler
    /// block itself is byte-identical.
    #[test]
    fn spec_hash_changes_when_top_level_const_edited() {
        let v1 = r#"spec Demo
const MAX = 100

handler foo (x : U64) : State.A -> State.A {
  requires state.count + x <= MAX
  effect { count += x }
}
"#;
        let v2 = r#"spec Demo
const MAX = 200

handler foo (x : U64) : State.A -> State.A {
  requires state.count + x <= MAX
  effect { count += x }
}
"#;
        let h1 = spec_hash_for_handler(v1, "foo").unwrap();
        let h2 = spec_hash_for_handler(v2, "foo").unwrap();
        assert_ne!(
            h1, h2,
            "top-level const change must invalidate handler spec_hash"
        );
    }

    /// Editing OTHER handlers must NOT invalidate this handler's spec_hash:
    /// each handler is sealed against its own block + shared top-level
    /// context only.
    #[test]
    fn spec_hash_stable_when_sibling_handler_edited() {
        let v1 = r#"spec Demo

handler foo : State.A -> State.A {
  effect { count += 1 }
}

handler bar : State.A -> State.B {
  effect { /* original */ }
}
"#;
        let v2 = r#"spec Demo

handler foo : State.A -> State.A {
  effect { count += 1 }
}

handler bar : State.A -> State.B {
  effect { count := 0; status := State.B }
}
"#;
        let h1 = spec_hash_for_handler(v1, "foo").unwrap();
        let h2 = spec_hash_for_handler(v2, "foo").unwrap();
        assert_eq!(
            h1, h2,
            "sibling handler edit must not invalidate this handler's spec_hash"
        );
    }

    #[test]
    fn spec_context_digest_deterministic() {
        let src = r#"spec Demo
const X = 1
type Account = | Active of { x : U64 }
"#;
        let d1 = spec_context_digest(src);
        let d2 = spec_context_digest(src);
        assert_eq!(d1, d2);
        assert_eq!(d1.len(), 16);
    }

    /// Mirrors `qedgen-macros::verified::tests::hash_deterministic` — drift
    /// on either side breaks both tests.
    #[test]
    fn body_hash_is_deterministic_and_16_hex() {
        let func: syn::ItemFn = syn::parse_quote! {
            pub fn deposit(amount: u64) -> u64 { amount + 1 }
        };
        let h1 = body_hash_for_fn(&func);
        let h2 = body_hash_for_fn(&func);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Mirrors `qedgen-macros::verified::tests::hash_ignores_attributes`.
    #[test]
    fn body_hash_ignores_outer_attributes() {
        let with_attr: syn::ItemFn = syn::parse_quote! {
            #[inline(always)]
            #[doc = "ignored"]
            pub fn deposit(amount: u64) -> u64 { amount + 1 }
        };
        let without_attr: syn::ItemFn = syn::parse_quote! {
            pub fn deposit(amount: u64) -> u64 { amount + 1 }
        };
        assert_eq!(
            body_hash_for_fn(&with_attr),
            body_hash_for_fn(&without_attr)
        );
    }

    /// Mirrors `qedgen-macros::verified::tests::hash_changes_on_body_change`.
    #[test]
    fn body_hash_changes_on_body_edit() {
        let v1: syn::ItemFn = syn::parse_quote! {
            pub fn deposit(amount: u64) -> u64 { amount + 1 }
        };
        let v2: syn::ItemFn = syn::parse_quote! {
            pub fn deposit(amount: u64) -> u64 { amount + 2 }
        };
        assert_ne!(body_hash_for_fn(&v1), body_hash_for_fn(&v2));
    }

    /// Cosmetic edits don't fire drift; semantic edits still do.
    #[test]
    fn spec_hash_is_whitespace_tolerant() {
        let h = spec_hash_for_handler(SAMPLE, "foo").unwrap();
        let reflowed = SAMPLE.replace("count += x", "count   +=   x");
        let h_reflowed = spec_hash_for_handler(&reflowed, "foo").unwrap();
        assert_eq!(h, h_reflowed);

        // Adding a line comment doesn't change the hash either.
        let with_comment = SAMPLE.replace("count += x", "// commentary\n    count += x");
        let h_commented = spec_hash_for_handler(&with_comment, "foo").unwrap();
        assert_eq!(h, h_commented);
    }

    #[test]
    fn spec_hash_still_changes_on_semantic_edit() {
        let h = spec_hash_for_handler(SAMPLE, "foo").unwrap();
        // Identifier change → must change hash.
        let renamed = SAMPLE.replace("count += x", "count += y");
        let h_renamed = spec_hash_for_handler(&renamed, "foo").unwrap();
        assert_ne!(h, h_renamed);
        // Operator change → must change hash.
        let op_changed = SAMPLE.replace("count += x", "count -= x");
        let h_op = spec_hash_for_handler(&op_changed, "foo").unwrap();
        assert_ne!(h, h_op);
    }

    #[test]
    fn normalize_preserves_string_literal_internal_whitespace() {
        // Spaces inside `"..."` are semantically meaningful and stay.
        let input = "  foo  \"hello   world\"  bar  ";
        assert_eq!(normalize_spec_block(input), "foo \"hello   world\" bar");
    }

    #[test]
    fn normalize_strips_block_comments() {
        let input = "foo /* inline comment */ bar";
        assert_eq!(normalize_spec_block(input), "foo bar");
    }

    /// Mirrors `qedgen-macros::verified::tests::fn_like_handles_method_shape_input`.
    #[test]
    fn body_hash_for_impl_fn_handles_self_receiver() {
        let func: syn::ImplItemFn = syn::parse_quote! {
            pub fn process(&mut self, lamports: u64) -> Result<()> {
                self.state.total_lamports += lamports;
                Ok(())
            }
        };
        let h = body_hash_for_impl_fn(&func);
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn accounts_struct_hash_finds_struct_and_is_stable() {
        let src = r#"
            use anchor_lang::prelude::*;

            #[derive(Accounts)]
            pub struct Buy<'info> {
                #[account(mut)]
                pub buyer: Signer<'info>,
                #[account(mut, has_one = mint)]
                pub vault: Account<'info, Vault>,
            }

            #[derive(Accounts)]
            pub struct Sell<'info> {
                pub seller: Signer<'info>,
            }
        "#;
        let h_buy = accounts_struct_hash(src, "Buy").unwrap();
        assert_eq!(h_buy.len(), 16);
        // Stable: same input → same hash.
        assert_eq!(accounts_struct_hash(src, "Buy").unwrap(), h_buy);
        // Different struct → different hash.
        let h_sell = accounts_struct_hash(src, "Sell").unwrap();
        assert_ne!(h_buy, h_sell);
        // Editing a constraint changes the hash.
        let edited = src.replace("#[account(mut)]", "#[account(mut, signer)]");
        assert_ne!(accounts_struct_hash(&edited, "Buy").unwrap(), h_buy);
    }

    #[test]
    fn accounts_struct_hash_returns_none_for_missing_struct() {
        let src = "pub struct Other { pub x: u64 }";
        assert!(accounts_struct_hash(src, "DoesNotExist").is_none());
    }

    /// A struct inside `pub mod accounts { ... }` resolves and hashes the
    /// same as top-level — only the struct's own tokens are hashed.
    #[test]
    fn accounts_struct_hash_descends_into_nested_mods() {
        let nested = r#"
            pub mod accounts {
                use anchor_lang::prelude::*;

                #[derive(Accounts)]
                pub struct Buy<'info> {
                    pub buyer: Signer<'info>,
                }
            }
        "#;
        let top_level = r#"
            use anchor_lang::prelude::*;

            #[derive(Accounts)]
            pub struct Buy<'info> {
                pub buyer: Signer<'info>,
            }
        "#;
        let h_nested = accounts_struct_hash(nested, "Buy").unwrap();
        let h_top = accounts_struct_hash(top_level, "Buy").unwrap();
        assert_eq!(h_nested, h_top);
    }

    #[test]
    fn accounts_struct_hash_handles_doubly_nested_mods() {
        let src = r#"
            pub mod a {
                pub mod b {
                    pub struct Buy { pub x: u64 }
                }
            }
        "#;
        let h = accounts_struct_hash(src, "Buy").unwrap();
        assert_eq!(h.len(), 16);
    }

    #[test]
    fn accounts_struct_hash_ignores_outer_attrs() {
        // Outer attrs are stripped before hashing on both sides, so derive
        // changes don't fire drift; inner field `#[account(...)]` attrs WILL
        // fire — they're part of the Field, not the outer struct.
        let with_attrs = r#"
            #[derive(Accounts, Debug, Clone)]
            pub struct Buy {
                pub x: u64,
            }
        "#;
        let without_attrs = r#"
            pub struct Buy {
                pub x: u64,
            }
        "#;
        assert_eq!(
            accounts_struct_hash(with_attrs, "Buy").unwrap(),
            accounts_struct_hash(without_attrs, "Buy").unwrap()
        );
    }

    #[test]
    fn block_comments_dont_unbalance() {
        let src = r#"
handler x : State.A -> State.A {
  /* a brace { in a block comment */
  effect { count += 1 }
}
"#;
        let block = extract_handler_block(src, "x").unwrap();
        assert!(block.contains("count += 1"));
    }
}
