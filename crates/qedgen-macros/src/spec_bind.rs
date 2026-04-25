//! Spec-binding extension for the `#[qed(verified, ...)]` attribute.
//!
//! At compile time, reads the `.qedspec` file referenced by the attribute,
//! extracts the raw text of `handler <name> { ... }` via balanced-brace
//! scanning, hashes that text (same SHA-256-hex16 algorithm used for
//! function-body drift), and compares to the declared `spec_hash`.
//!
//! Pure compile-time check: the wrapped function is emitted byte-identical
//! to what the user wrote.

use proc_macro2::TokenStream;
use sha2::{Digest, Sha256};

use crate::verified::FnLike;

/// Parsed attribute arguments from `#[qed(verified, spec=..., handler=..., ...)]`.
pub(crate) struct Args {
    pub spec: Option<String>,
    pub handler: Option<String>,
    pub hash: Option<String>,
    pub spec_hash: Option<String>,
    /// Optional `#[derive(Accounts)]` struct name (e.g. `Buy`,
    /// `Initialize`). Present when the user wants the macro to also
    /// hash-check the accounts struct alongside the body + spec.
    pub accounts: Option<String>,
    /// Path to the file declaring the accounts struct, relative to
    /// `CARGO_MANIFEST_DIR`. Anchor scaffold typically puts it in
    /// `src/lib.rs`; some programs split into `src/accounts.rs` or
    /// `src/instructions/<name>.rs`.
    pub accounts_file: Option<String>,
    /// Sealed hash of the canonicalized accounts struct (sha256-hex16
    /// of the syn ItemStruct's tokens after attribute stripping).
    pub accounts_hash: Option<String>,
}

/// SHA-256 hash of a string, truncated to 16 hex characters.
/// Shared algorithm with `verified::sha256_hex16` (kept private there).
fn sha256_hex16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Parse all `key = "value"` pairs from the attribute stream.
pub(crate) fn parse_args(attr: &TokenStream) -> Result<Args, syn::Error> {
    let tokens: Vec<proc_macro2::TokenTree> = attr.clone().into_iter().collect();

    let mut spec = None;
    let mut handler = None;
    let mut hash = None;
    let mut spec_hash = None;
    let mut accounts = None;
    let mut accounts_file = None;
    let mut accounts_hash = None;

    let mut i = 0;
    while i < tokens.len() {
        if let proc_macro2::TokenTree::Ident(ref ident) = tokens[i] {
            let name = ident.to_string();
            if matches!(
                name.as_str(),
                "spec"
                    | "handler"
                    | "hash"
                    | "spec_hash"
                    | "accounts"
                    | "accounts_file"
                    | "accounts_hash"
            ) {
                // Expect `=` then literal
                if i + 2 >= tokens.len() {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected `{} = \"...\"`", name),
                    ));
                }
                let eq_ok = matches!(&tokens[i + 1], proc_macro2::TokenTree::Punct(p) if p.as_char() == '=');
                if !eq_ok {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected `=` after `{}`", name),
                    ));
                }
                let value = if let proc_macro2::TokenTree::Literal(ref lit) = tokens[i + 2] {
                    let lit_str = lit.to_string();
                    lit_str.trim_matches('"').to_string()
                } else {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected string literal for `{}`", name),
                    ));
                };
                if value.is_empty() {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): `{}` value cannot be empty", name),
                    ));
                }
                match name.as_str() {
                    "spec" => spec = Some(value),
                    "handler" => handler = Some(value),
                    "hash" => hash = Some(value),
                    "spec_hash" => spec_hash = Some(value),
                    "accounts" => accounts = Some(value),
                    "accounts_file" => accounts_file = Some(value),
                    "accounts_hash" => accounts_hash = Some(value),
                    _ => unreachable!(),
                }
                i += 3;
                continue;
            }
        }
        i += 1;
    }

    Ok(Args {
        spec,
        handler,
        hash,
        spec_hash,
        accounts,
        accounts_file,
        accounts_hash,
    })
}

/// Parse `source` as Rust, find a top-level `pub struct <name>`, and
/// return the canonical hash of its tokens (after outer-attribute
/// stripping). Mirrors `qedgen::spec_hash::accounts_struct_hash` so
/// the proc-macro and the qedgen-side computation produce identical
/// values; any divergence yields a spurious accounts-hash drift.
///
/// Returns `None` when:
///   - the file isn't valid Rust source
///   - no `struct <name>` is declared at the top level (we don't
///     descend into mods to keep the search predictable)
pub(crate) fn accounts_struct_hash_in(source: &str, struct_name: &str) -> Option<String> {
    use quote::ToTokens;
    let file: syn::File = syn::parse_str(source).ok()?;
    for item in file.items {
        if let syn::Item::Struct(mut s) = item {
            if s.ident == struct_name {
                s.attrs.clear();
                let canonical = s.to_token_stream().to_string();
                return Some(sha256_hex16(&canonical));
            }
        }
    }
    None
}

/// Extract the raw text of a `handler <name> { ... }` block from the spec source
/// by scanning for the keyword + balanced-brace matching.
///
/// Returns the body (including the enclosing braces). Whitespace and comments
/// inside are preserved — the hash is of the raw bytes of that slice.
pub(crate) fn extract_handler_block(source: &str, handler_name: &str) -> Option<String> {
    // Match `handler <name>` where <name> is followed by a word boundary.
    // Hand-rolled to avoid pulling regex into the proc-macro crate.
    let needle = "handler";
    let mut search_from = 0;
    while let Some(pos) = source[search_from..].find(needle) {
        let abs = search_from + pos;
        // Require that the previous char is whitespace or SOF
        let prev_ok = abs == 0
            || source.as_bytes()[abs - 1].is_ascii_whitespace()
            || source.as_bytes()[abs - 1] == b'\n';
        // Require that the char after 'handler' is whitespace
        let after = abs + needle.len();
        if !prev_ok || after >= source.len() || !source.as_bytes()[after].is_ascii_whitespace() {
            search_from = abs + 1;
            continue;
        }
        // Skip whitespace, then capture the identifier
        let rest = &source[after..];
        let rest_trimmed = rest.trim_start();
        let ws_consumed = rest.len() - rest_trimmed.len();
        // Capture identifier bytes: ASCII alnum + underscore.
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
        // Found the handler. Now scan forward to the first `{` and do balanced
        // matching (respecting single-line `//` comments and string literals —
        // since the DSL allows `//` comments inside handler bodies, we must
        // not count `{`/`}` that appear in comments).
        let body_search_start = after + ws_consumed + id_end;
        let body_bytes = source.as_bytes();
        let mut cursor = body_search_start;
        // Find the opening brace.
        while cursor < body_bytes.len() && body_bytes[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= body_bytes.len() {
            return None;
        }
        let block_start = cursor;
        let mut depth = 0i32;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut in_str = false;
        while cursor < body_bytes.len() {
            let b = body_bytes[cursor];
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                }
                cursor += 1;
                continue;
            }
            if in_block_comment {
                if b == b'*' && cursor + 1 < body_bytes.len() && body_bytes[cursor + 1] == b'/' {
                    in_block_comment = false;
                    cursor += 2;
                    continue;
                }
                cursor += 1;
                continue;
            }
            if in_str {
                if b == b'\\' && cursor + 1 < body_bytes.len() {
                    cursor += 2;
                    continue;
                }
                if b == b'"' {
                    in_str = false;
                }
                cursor += 1;
                continue;
            }
            // Check for comment/string starts
            if b == b'/' && cursor + 1 < body_bytes.len() {
                let nxt = body_bytes[cursor + 1];
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
                    // Found the matching close brace.
                    let block_end = cursor + 1;
                    return Some(source[block_start..block_end].to_string());
                }
            }
            cursor += 1;
        }
        return None;
    }
    None
}

/// Compute the spec hash for a handler: extract the block text, then sha256-hex16 it.
pub(crate) fn spec_hash_for_handler(source: &str, handler_name: &str) -> Option<String> {
    extract_handler_block(source, handler_name).map(|s| sha256_hex16(&s))
}

/// Main expansion for `#[qed(verified, spec=..., handler=..., hash=..., spec_hash=...)]`.
///
/// If `spec` and `handler` are both absent, falls back to the legacy
/// body-only flow in `verified::expand`. Otherwise performs both the
/// body-hash check AND the spec-hash check at compile time.
///
/// The macro NEVER injects runtime code — expansion is the original function
/// alone (or compile_error + original function on drift).
pub fn expand_bound(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the item — accepts free fns and impl methods alike.
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

    let args = match parse_args(&attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error(),
    };

    // If spec/handler not both present, fall back to the body-only path.
    let (spec_path, handler_name) = match (&args.spec, &args.handler) {
        (Some(s), Some(h)) => (s.clone(), h.clone()),
        _ => return crate::verified::expand(attr, item),
    };

    let fn_name = func.ident().to_string();
    let actual_body_hash = func.content_hash();
    let func_span = func.name_span();
    let func_tokens = func.to_token_stream();

    // Locate the spec file relative to CARGO_MANIFEST_DIR.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let full_path = std::path::Path::new(&manifest_dir).join(&spec_path);
    let source = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "qed(verified): could not read spec file `{}` (resolved to `{}`): {}",
                spec_path,
                full_path.display(),
                e
            );
            let err = syn::Error::new(func_span, msg).to_compile_error();
            return quote::quote! { #err #func_tokens };
        }
    };

    let actual_spec_hash = match spec_hash_for_handler(&source, &handler_name) {
        Some(h) => h,
        None => {
            let msg = format!(
                "qed(verified): handler `{}` not found in spec `{}`",
                handler_name, spec_path,
            );
            let err = syn::Error::new(func_span, msg).to_compile_error();
            return quote::quote! { #err #func_tokens };
        }
    };

    // Optional accounts-struct check. When all three of `accounts`,
    // `accounts_file`, and `accounts_hash` are present, hash the
    // referenced struct and validate. Absent → backward-compat
    // body+spec-only check.
    let actual_accounts_hash = match (
        args.accounts.as_deref(),
        args.accounts_file.as_deref(),
        args.accounts_hash.as_deref(),
    ) {
        (Some(name), Some(file), _) => {
            let acct_path = std::path::Path::new(&manifest_dir).join(file);
            match std::fs::read_to_string(&acct_path) {
                Ok(src) => match accounts_struct_hash_in(&src, name) {
                    Some(h) => Some(h),
                    None => {
                        let msg = format!(
                            "qed(verified): accounts struct `{}` not found in `{}` (resolved to `{}`)",
                            name, file, acct_path.display(),
                        );
                        let err = syn::Error::new(func_span, msg).to_compile_error();
                        return quote::quote! { #err #func_tokens };
                    }
                },
                Err(e) => {
                    let msg = format!(
                        "qed(verified): could not read accounts file `{}` (resolved to `{}`): {}",
                        file,
                        acct_path.display(),
                        e
                    );
                    let err = syn::Error::new(func_span, msg).to_compile_error();
                    return quote::quote! { #err #func_tokens };
                }
            }
        }
        _ => None,
    };

    // Both core hashes must be provided for a sealed attribute. The
    // accounts-hash leg is optional (legacy attributes still work).
    match (args.hash.as_deref(), args.spec_hash.as_deref()) {
        (Some(body_expected), Some(spec_expected)) => {
            if body_expected != actual_body_hash {
                let msg = format!(
                    "qed: verified function `{}` has changed since verification \
                     — re-verify or update hash.\n\
                     Expected: {}\n\
                     Actual:   {}",
                    fn_name, body_expected, actual_body_hash
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
            if spec_expected != actual_spec_hash {
                let msg = format!(
                    "qed: handler `{}` spec contract changed.\n\
                     Expected: {}\n\
                     Actual:   {}\n\
                     Re-run `qedgen check` for a diff.",
                    handler_name, spec_expected, actual_spec_hash
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
            // If accounts metadata supplied, validate that leg too.
            if let (Some(acct_name), Some(acct_actual), Some(acct_expected)) = (
                args.accounts.as_deref(),
                actual_accounts_hash.as_deref(),
                args.accounts_hash.as_deref(),
            ) {
                if acct_expected != acct_actual {
                    let msg = format!(
                        "qed: accounts struct `{}` changed since verification \
                         (handler `{}`).\n\
                         Expected: {}\n\
                         Actual:   {}\n\
                         Re-run `qedgen adapt --spec ...` to refresh.",
                        acct_name, handler_name, acct_expected, acct_actual
                    );
                    let err = syn::Error::new(func_span, msg).to_compile_error();
                    return quote::quote! { #err #func_tokens };
                }
            }
            // All match — pass through.
            item
        }
        _ => {
            // Setup mode: emit the computed hashes for copy-paste.
            let accounts_line = match (
                args.accounts.as_deref(),
                args.accounts_file.as_deref(),
                actual_accounts_hash.as_deref(),
            ) {
                (Some(name), Some(file), Some(h)) => format!(
                    ", accounts = \"{}\", accounts_file = \"{}\", accounts_hash = \"{}\"",
                    name, file, h
                ),
                _ => String::new(),
            };
            let msg = format!(
                "qed(verified): no hash(es) provided for `{}`.\n\
                 Computed body hash: {}\n\
                 Computed spec hash: {}\n\
                 Usage: #[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\"{})]",
                fn_name,
                actual_body_hash,
                actual_spec_hash,
                spec_path,
                handler_name,
                actual_body_hash,
                actual_spec_hash,
                accounts_line,
            );
            let err = syn::Error::new(func_span, msg).to_compile_error();
            quote::quote! { #err #func_tokens }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SPEC: &str = r#"
spec Percolator

handler deposit (i : AccountIdx) (amount : U128) : State.Active -> State.Active {
  requires state.accounts[i].active == 1 else SlotInactive
  effect {
    V += amount
    accounts[i].capital += amount
  }
}

handler withdraw (i : AccountIdx) (amount : U128) : State.Active -> State.Active {
  requires state.accounts[i].capital >= amount else InsufficientFunds
  effect {
    V -= amount
    accounts[i].capital -= amount
  }
}
"#;

    #[test]
    fn extract_deposit() {
        let block = extract_handler_block(SAMPLE_SPEC, "deposit").unwrap();
        assert!(block.starts_with('{'));
        assert!(block.ends_with('}'));
        assert!(block.contains("active == 1"));
        assert!(block.contains("V += amount"));
        // Should NOT pull in withdraw.
        assert!(!block.contains("withdraw"));
        assert!(!block.contains("capital >= amount"));
    }

    #[test]
    fn extract_withdraw() {
        let block = extract_handler_block(SAMPLE_SPEC, "withdraw").unwrap();
        assert!(block.contains("capital >= amount"));
        assert!(!block.contains("active == 1"));
    }

    #[test]
    fn missing_handler() {
        assert!(extract_handler_block(SAMPLE_SPEC, "nonexistent").is_none());
    }

    #[test]
    fn spec_hash_stable() {
        let h1 = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let h2 = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn spec_hash_different_for_different_handlers() {
        let h_dep = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let h_wit = spec_hash_for_handler(SAMPLE_SPEC, "withdraw").unwrap();
        assert_ne!(h_dep, h_wit);
    }

    #[test]
    fn spec_hash_changes_on_edit() {
        let h_orig = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let edited = SAMPLE_SPEC.replace("V += amount", "V += amount + 1");
        let h_edit = spec_hash_for_handler(&edited, "deposit").unwrap();
        assert_ne!(h_orig, h_edit);
    }

    #[test]
    fn spec_hash_whitespace_sensitive() {
        // Whitespace inside the block is part of the extracted text. We
        // choose sensitivity over normalisation so minor reformats are
        // surfaced by the same compile_error that catches real edits.
        let h_orig = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let spaced = SAMPLE_SPEC.replace("active == 1", "active  ==  1");
        let h_spaced = spec_hash_for_handler(&spaced, "deposit").unwrap();
        assert_ne!(h_orig, h_spaced);
    }

    #[test]
    fn parse_all_args() {
        let attr: TokenStream = quote::quote! {
            verified,
            spec = "percolator.qedspec",
            handler = "deposit",
            hash = "aaaaaaaaaaaaaaaa",
            spec_hash = "bbbbbbbbbbbbbbbb"
        };
        let args = parse_args(&attr).unwrap();
        assert_eq!(args.spec.as_deref(), Some("percolator.qedspec"));
        assert_eq!(args.handler.as_deref(), Some("deposit"));
        assert_eq!(args.hash.as_deref(), Some("aaaaaaaaaaaaaaaa"));
        assert_eq!(args.spec_hash.as_deref(), Some("bbbbbbbbbbbbbbbb"));
    }

    #[test]
    fn parse_legacy_no_spec() {
        let attr: TokenStream = quote::quote! { verified, hash = "abc123def456789a" };
        let args = parse_args(&attr).unwrap();
        assert_eq!(args.spec, None);
        assert_eq!(args.handler, None);
        assert_eq!(args.hash.as_deref(), Some("abc123def456789a"));
        assert_eq!(args.spec_hash, None);
    }

    #[test]
    fn extract_handles_nested_braces() {
        let src = r#"
handler complex : State.Active -> State.Active {
  effect {
    if cond {
      x := 1
    }
  }
}
"#;
        let block = extract_handler_block(src, "complex").unwrap();
        assert!(block.contains("x := 1"));
        // Must include both inner closing braces.
        assert_eq!(block.matches('{').count(), block.matches('}').count());
    }

    #[test]
    fn extract_ignores_braces_in_comments() {
        let src = r#"
handler commented : State.Active -> State.Active {
  // This has a { brace in a comment
  effect {
    x := 1
  }
}
"#;
        let block = extract_handler_block(src, "commented").unwrap();
        assert!(block.contains("x := 1"));
        // The '{' inside the `// …` line should not unbalance the scanner.
        assert!(block.ends_with('}'));
    }
}
