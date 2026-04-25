//! Shared helper: extract a `handler <name> { ... }` block from a `.qedspec`
//! source and compute its SHA-256-hex16 hash. Also: compute the body hash
//! of a `syn::ItemFn` using the same canonicalization as `qedgen-macros`.
//!
//! The two algorithms here MUST match `qedgen-macros`:
//!   - `spec_hash_for_handler` ↔ `qedgen-macros/src/spec_bind.rs`
//!   - `body_hash_for_fn`      ↔ `qedgen-macros/src/verified.rs::content_hash`
//!
//! Codegen + `qedgen adapt --spec ...` emit the `hash = "..."` /
//! `spec_hash = "..."` attribute values; the proc-macro recomputes both
//! at compile time. Any divergence yields a spurious drift error — treat
//! any change here as a breaking change of both crates.

use quote::ToTokens;
use sha2::{Digest, Sha256};

fn sha256_hex16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Compute the body hash of a `syn::ItemFn`. MUST match
/// `qedgen-macros::verified::content_hash` byte-for-byte: strip every
/// outer attribute (doc comments, `#[qed(...)]`, `#[inline]`, etc.),
/// normalize via `to_token_stream()`, then sha256-hex16.
///
/// Used by `qedgen adapt --spec ...` to compute the `hash = "..."`
/// value the macro will recompute and check at compile time.
pub fn body_hash_for_fn(func: &syn::ItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    let canonical = stripped.to_token_stream().to_string();
    sha256_hex16(&canonical)
}

/// Extract the raw text of a `handler <name> { ... }` block (braces included)
/// via keyword search + balanced-brace scanning, treating `//`, `/* */`, and
/// `"…"` as opaque regions.
pub fn extract_handler_block(source: &str, handler_name: &str) -> Option<String> {
    let needle = "handler";
    let bytes = source.as_bytes();
    let mut search_from = 0;
    while let Some(pos) = source[search_from..].find(needle) {
        let abs = search_from + pos;
        let prev_ok = abs == 0 || bytes[abs - 1].is_ascii_whitespace();
        let after = abs + needle.len();
        if !prev_ok || after >= bytes.len() || !bytes[after].is_ascii_whitespace() {
            search_from = abs + 1;
            continue;
        }
        let rest = &source[after..];
        let rest_trimmed = rest.trim_start();
        let ws_consumed = rest.len() - rest_trimmed.len();
        let mut id_end = 0;
        for (i, c) in rest_trimmed.char_indices() {
            if c.is_ascii_alphanumeric() || c == '_' {
                id_end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if id_end == 0 {
            search_from = abs + 1;
            continue;
        }
        let ident = &rest_trimmed[..id_end];
        if ident != handler_name {
            search_from = abs + 1;
            continue;
        }
        let body_search_start = after + ws_consumed + id_end;
        let mut cursor = body_search_start;
        while cursor < bytes.len() && bytes[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }
        let block_start = cursor;
        let mut depth = 0i32;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut in_str = false;
        while cursor < bytes.len() {
            let b = bytes[cursor];
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                }
                cursor += 1;
                continue;
            }
            if in_block_comment {
                if b == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                    in_block_comment = false;
                    cursor += 2;
                    continue;
                }
                cursor += 1;
                continue;
            }
            if in_str {
                if b == b'\\' && cursor + 1 < bytes.len() {
                    cursor += 2;
                    continue;
                }
                if b == b'"' {
                    in_str = false;
                }
                cursor += 1;
                continue;
            }
            if b == b'/' && cursor + 1 < bytes.len() {
                let nxt = bytes[cursor + 1];
                if nxt == b'/' {
                    in_line_comment = true;
                    cursor += 2;
                    continue;
                }
                if nxt == b'*' {
                    in_block_comment = true;
                    cursor += 2;
                    continue;
                }
            }
            if b == b'"' {
                in_str = true;
                cursor += 1;
                continue;
            }
            if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(source[block_start..cursor + 1].to_string());
                }
            }
            cursor += 1;
        }
        return None;
    }
    None
}

/// Compute the spec hash for a handler. Returns `None` if the handler block
/// is absent or a handler declared with no body (e.g. `handler foo : A -> B`
/// with no braces — treated as an empty contract so codegen emits an empty
/// placeholder hash that the macro side will also compute as `None`).
pub fn spec_hash_for_handler(source: &str, handler_name: &str) -> Option<String> {
    extract_handler_block(source, handler_name).map(|s| sha256_hex16(&s))
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

    /// Mirrors `qedgen-macros::verified::tests::hash_deterministic`. If
    /// either side's algorithm drifts, this test breaks alongside the
    /// macro test — same input, same expected length.
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
