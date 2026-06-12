//! Handler-block extraction + SHA-256-hex16 spec/body hashing.
//!
//! The algorithms here MUST match `qedgen-macros` (which recomputes the
//! emitted `hash = "..."` / `spec_hash = "..."` values at compile time):
//!   - `spec_hash_for_handler` ↔ `qedgen-macros/src/spec_bind.rs`
//!   - `body_hash_for_fn`      ↔ `qedgen-macros/src/verified.rs::content_hash`
//!
//! Any divergence yields spurious drift — a change here breaks both crates.

use quote::ToTokens;
use sha2::{Digest, Sha256};

fn sha256_hex16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Body hash of a `syn::ItemFn`. MUST match
/// `qedgen-macros::verified::content_hash` byte-for-byte: strip all outer
/// attributes, normalize via `to_token_stream()`, sha256-hex16.
pub fn body_hash_for_fn(func: &syn::ItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    sha256_hex16(&canonical_token_string(&stripped.to_token_stream()))
}

/// Body hash for an impl method; same algorithm as `body_hash_for_fn`.
/// Mirrors `qedgen-macros::verified::FnLike`'s Impl-arm hash (method-shape
/// `#[qed]` annotations).
pub fn body_hash_for_impl_fn(func: &syn::ImplItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    sha256_hex16(&canonical_token_string(&stripped.to_token_stream()))
}

/// Canonical token string: each token in order, single-space separated.
/// MUST mirror `qedgen-macros::canonical_token_string` byte-for-byte —
/// rustc-vs-from_str spacing divergence forces this hand-rolled traversal.
fn canonical_token_string(stream: &proc_macro2::TokenStream) -> String {
    use proc_macro2::{Delimiter, TokenTree};
    let mut out = String::new();
    fn walk(stream: proc_macro2::TokenStream, out: &mut String) {
        for tt in stream {
            match tt {
                TokenTree::Group(g) => {
                    let (open, close) = match g.delimiter() {
                        Delimiter::Brace => ('{', '}'),
                        Delimiter::Bracket => ('[', ']'),
                        Delimiter::Parenthesis => ('(', ')'),
                        Delimiter::None => (' ', ' '),
                    };
                    if g.delimiter() != Delimiter::None {
                        out.push(open);
                        out.push(' ');
                    }
                    walk(g.stream(), out);
                    if g.delimiter() != Delimiter::None {
                        out.push(close);
                        out.push(' ');
                    }
                }
                TokenTree::Ident(i) => {
                    out.push_str(&i.to_string());
                    out.push(' ');
                }
                TokenTree::Literal(l) => {
                    out.push_str(&l.to_string());
                    out.push(' ');
                }
                TokenTree::Punct(p) => {
                    out.push(p.as_char());
                    out.push(' ');
                }
            }
        }
    }
    walk(stream.clone(), &mut out);
    out
}

/// Hash a `pub struct <name>` from Rust source. MUST match
/// `qedgen-macros::spec_bind::accounts_struct_hash_in`. Seals the handler's
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
                let canonical = canonical_token_string(&stripped.to_token_stream());
                return Some(sha256_hex16(&canonical));
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

/// Normalize a handler block before hashing so cosmetic edits don't fire
/// drift while semantic edits do: strip `//` and `/* */` comments, collapse
/// whitespace runs outside strings to one space, trim; string literals
/// (incl. `\"` escapes) pass through verbatim — interior spaces are
/// semantic. MUST match `qedgen-macros::spec_bind::normalize_spec_block`.
pub fn normalize_spec_block(block: &str) -> String {
    let bytes = block.as_bytes();
    let mut out = String::with_capacity(block.len());
    let mut i = 0;
    let mut in_str = false;
    let mut last_emit_was_ws = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            last_emit_was_ws = false;
            continue;
        }
        // Line comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            // The newline ends the comment; fall through so the
            // whitespace-collapse arm below treats it as a separator.
            continue;
        }
        // Block comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = i.saturating_add(2);
            // Treat the comment gap as a single whitespace separator
            // unless we'd otherwise emit two spaces in a row.
            if !out.is_empty() && !last_emit_was_ws {
                out.push(' ');
                last_emit_was_ws = true;
            }
            continue;
        }
        if b == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            last_emit_was_ws = false;
            continue;
        }
        if b.is_ascii_whitespace() {
            if !out.is_empty() && !last_emit_was_ws {
                out.push(' ');
                last_emit_was_ws = true;
            }
            i += 1;
            continue;
        }
        out.push(b as char);
        last_emit_was_ws = false;
        i += 1;
    }
    out.trim().to_string()
}

/// Digest of everything in `source` *except* handler blocks. Handler blocks
/// are sealed individually by `spec_hash_for_handler`; all other top-level
/// items are shared context that changes every handler's effective contract,
/// so this digest is folded into each handler's spec_hash.
///
/// Algorithm: balanced-brace scan (as `extract_handler_block`, collecting
/// all ranges), remove handler ranges, normalize, sha256-hex16.
///
/// Conservative-by-design: a top-level change NO handler references still
/// invalidates every hash — over-invalidates vs. dataflow analysis, but
/// simple and deterministic.
///
/// MUST mirror `qedgen-macros::spec_bind::spec_context_digest`.
pub fn spec_context_digest(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut search_from = 0;
    let mut last_emit = 0usize;
    let needle = "handler";

    while let Some(pos) = source[search_from..].find(needle) {
        let abs = search_from + pos;
        let prev_ok = abs == 0 || bytes[abs - 1].is_ascii_whitespace();
        let after = abs + needle.len();
        if !prev_ok || after >= bytes.len() || !bytes[after].is_ascii_whitespace() {
            search_from = abs + 1;
            continue;
        }
        // Skip past `handler <name>` to find the opening brace, if any.
        let mut cursor = after;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        // Identifier
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        // Whitespace + optional `(...)` params + `:` + lifecycle clause —
        // everything up to the first `{` that opens the body.
        while cursor < bytes.len() && bytes[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            search_from = abs + 1;
            continue;
        }
        let block_start = abs;
        let body_start = cursor;
        let mut depth = 0i32;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut in_str = false;
        cursor = body_start;
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
                    let block_end = cursor + 1;
                    out.push_str(&source[last_emit..block_start]);
                    out.push(' ');
                    last_emit = block_end;
                    search_from = block_end;
                    break;
                }
            }
            cursor += 1;
        }
        if depth != 0 {
            // Unterminated handler block — bail out, hash what we have.
            break;
        }
    }
    out.push_str(&source[last_emit..]);
    sha256_hex16(&normalize_spec_block(&out))
}

/// Spec hash for a handler. `None` if the block is absent or the handler is
/// bodyless (`handler foo : A -> B` with no braces — an empty contract; the
/// macro side also computes `None`). The block is normalized before hashing,
/// and `spec_context_digest(source)` is folded in so top-level shared
/// declarations propagate into every handler's hash.
pub fn spec_hash_for_handler(source: &str, handler_name: &str) -> Option<String> {
    let block = extract_handler_block(source, handler_name)?;
    let normalized = normalize_spec_block(&block);
    let context = spec_context_digest(source);
    Some(sha256_hex16(&format!("{}:{}", normalized, context)))
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
